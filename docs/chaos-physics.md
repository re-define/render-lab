# Chaos 刚体、碰撞与破碎算法

本文沿 UE 5.8 分支的固定提交 `f97cae4acf019789ed431e0ac28d47e15422784c` 追踪世界刚体求解器 `FPBDRigidsSolver` 及其 `FPBDRigidsEvolutionGBF`。接触公式以可直接对照的标量 Gauss–Seidel 路径为准。具体工程可选 SIMD、Partial Jacobi、CCD 模式及不同损伤模型。源码链接需要 Epic GitHub 仓库访问权限。 [求解器 API][rigids] · [求解路径选择][solver-mode]

## 1. 一个物理子步从哪开始

游戏线程经物理代理提交对象、形状、运动学目标和命令。`FChaosMarshallingManager` 给输入包分配外部时间戳、内部步编号和区间序号。一个外部包可生成多个子步包，将有持续效力的数据传给后续子步。物理线程应用已消费的输入到自己的粒子存储，读取并清除一次冲量项。求解结束后输出带模拟起止时刻和已消费时间戳的脏结果。异步显示可在相邻已发布结果之间插值。游戏线程和求解线程如何交接，与一个求解步内有多少工作任务，是两条独立的调度链。 [推送包与拉取包][marshal] · [输入消费][push-apply] · [结果缓冲][buffer] · [单体代理插值][pull] · [子步设置][substep]

~~~text
消费本步输入
并行积分动态体，应用运动学目标，更新空间索引
宽相收集候选，中相管理形状对，窄相计算接触流形
处理扫掠接触与 CCD
更新约束图，组装和求解岛
做求解后的 CCD 校正，处理睡眠与破碎
捕获所需历史，提交粒子姿态并发布脏结果
~~~

实际步骤还穿插回调、图维护、修正与调试采集。下面按有效数据依赖展开。 [Evolution 子步前半][advance] · [求解与收尾][finish]

## 2. 积分、宽相与 GJK

### 2.1 用速度预测质心姿态

粒子分别保留步初姿态 `X/R` 与预测姿态 `P/Q`。积分在质心空间推进。简化写法是 `v₁ = v₀ + aΔt + Δv_impulse`，角速度同理；清空冲量输入后再应用阻尼、可选陀螺修正和速度上限。预测质心用 `P = X_com + v₁Δt`，`Q` 用角速度积分当前质心旋转。运动学刚体按目标分段推进，作为动态体的移动边界。派生形状姿态和预测 AABB 可沿速度扩张，快速运动的 CCD 体还可生成扫掠包围。 [积分和包围][integrate] · [运动学目标][kinematic]

### 2.2 从可能相交的粒子到接触点

| 阶段 | 算法与结果 |
| --- | --- |
| 宽相 | 先在内部空间结构查询 AABB 重叠，以粒子状态与过滤条件排除不需检测的对。上下文收集候选，再找或建相应粒子对的 midphase。 |
| 中相 | 将一个粒子对拆成可能接触的形状对。复合形状可遍历子形状或局部隐式 BVH。按形状对维护窄相缓存，以免每步重建接触对象和稳定摩擦锚点。 |
| 窄相 | 根据形状组合调用专门的球、胶囊、凸体、三角面等算法。保存形状局部接触点、法线与分离量，重用仍有效的流形，必要时重算。 |
| CCD 提交 | 对穿越风险较高的形状对，求出扫掠接触时刻及其约束，另交给 CCD manager，之后收敛到常规约束数据。 |

这些阶段有不同工作量指标。宽相增多说明潜在体对增多。稳定接触缓存命中高时，中相对象数与本步实际窄相调用数也可能不同。 [宽相重叠][broad] · [中相的 BVH 分派][mid-bvh] · [复用和更新流形][manifold] · [扫掠接触][swept-contact]

**一个可对照的窄相实例。** 球对凸体先把球心变到凸体空间，对凸体与半径为零的球心点做 GJK 距离测试，再把真实球半径计入接触。在常见的分离路径，GJK 用支持点 `S_A(d)−S_B(−d)` 隐式访问 Minkowski 差，通过单纯形上距原点最近的点迭代更新方向，重心坐标也能回推出两形体上的最近点。对球心深埋的情况，这个形状特化会检查凸体平面以构造接触。其他形状对还有自己的流形算法。GJK 扫掠的若干初始重叠路径可调用 EPA 估计分离方向及深度。 [球对凸体分支][sphere-convex] · [GJK 距离算法][gjk] · [扫掠中的 EPA 分支][epa]

## 3. CCD 怎样处理跨过物体的一步

离散检测只检查步末，薄障碍可能被高速体一步跨过。扫掠检测给出步内归一化的碰撞时刻 TOI。管理器先把共享动态体的扫掠约束组成临时 CCD 岛，再在每岛内按 TOI 升序处理。多次处理路径在最早事件处将相关体推进到碰撞时刻，应用响应，并根据剩余时间重新扫掠受影响的邻接约束。速度或预测终点改变会让后续 TOI 更新，余下事件需要重排。约束有处理次数上限，达到上限时可裁剪相关体的剩余运动。不同 CCD 岛可以并行。设置单次处理时走更简单的循环，后续离散求解负责剩余接触。完成时更新受影响的常规约束，Evolution 在主求解后还可做 CCD 校正。 [CCD 分组及模式][ccd-mode] · [事件顺序与响应][ccd-order] · [重扫并更新 TOI][ccd-resweep] · [Evolution 中的调用][advance-ccd]

## 4. 约束岛怎样分工

约束图以刚体为节点，以接触或关节为边。共享可写动态体的约束留在一个岛。两个岛可共用只读的静态地面或运动学边界。Island Manager 增量维护图。新边可把已有岛加入合并集，删边后只检查需要分裂的岛，沿剩余边重新找连通分量。这样，静态地面上的多堆物体可分别求解，而相互碰撞的堆会合并。 [岛和运动学边界][island] · [合并及分裂][island-updates]

组管理器按约束量划分工作，将许多小岛放入组。任务式路径先 gather bodies，再 gather constraints，之后 solve、scatter constraints、scatter bodies。约束必须等求解体数据准备好，约束写回也可能读取求解体的修正姿态，因此这条路径有对应的依赖。Gauss–Seidel 迭代按顺序用前一条约束的最新修正处理下一条。大岛内若要再并行，必须按不相交体集分批或使用具有独立累积语义的变体。 [组预算][groups] · [任务依赖][gather] · [顺序迭代][group-solve]

## 5. 一条接触怎样推出位移、速度与摩擦

以下用一条接触和单位法线 `n` 描述标量路径，选择法线由体 1 指向体 0。忽略可选软层、分离偏移或分裂冲量时，位置算法可直接写成下述式子。 [世界接触与目标速度][contact-gather] · [标量求解器][position]

### 5.1 有效质量和位置约束

两个形状的局部接触点先变到世界坐标，求解器取共同接触位置，记录从两质心到它的力臂 `r₀,r₁`。沿方向 `d` 的逆有效质量为

~~~text
k(d) = inv_m0 + inv_m1
     + (r0 × d) · inv_I0 (r0 × d)
     + (r1 × d) · inv_I1 (r1 × d)
M(d) = 1 / k(d)   # k 足够大时
~~~

静态／运动学端的不可写自由度不给有效质量贡献。平移相同的两个物体，偏离质心的接触还会引起转动，因此不能只按质量分摊。相同计算也用于切向的两个基向量。 [法向与切向有效质量][mass]

定义 `Cₙ` 为本轮包含已有修正的法向误差，互穿时为负。方向性累积位置乘子 `λₙ` 的简化更新为

~~~text
Δλ_raw = −stiffness × M(n) × Cn
λ_next = max(0, λ_old + Δλ_raw)
Δλ     = λ_next − λ_old
~~~

体 0 加 `inv_m0 × Δλ n`，体 1 减相应量，角向沿 `inv_I(r × n)` 更新。分离后允许撤销前一轮过大的推出量，累计量仍受单侧约束。举例，两个 1 kg 物体的质心正碰，侵入 2 cm、刚度取 1 且忽略其他选项时，逆有效质量是 2，计算出的修正令双方沿法线各移动 1 cm。多接触时误差会受相邻约束后续修正影响，需多轮迭代。 [位置推出与单侧截断][position] · [推出作用到位姿][pos-apply]

### 5.2 静摩擦、先前锚点与摩擦锥

接触可保留上一时刻的局部锚点以估计沿切面的偏移，缺少可靠锚点时可以按相对接触速度乘 `Δt` 估计。位置阶段先算法向，再沿两个切向求校正，比较累计切向量和 `μ_s × λ_normal` 的界限。超过静摩擦界限时，沿原切向方向缩放到动摩擦界限 `μ_d × λ_normal`。实现还包含最小摩擦基准和仅在最后若干位置迭代启用摩擦的设置。 [摩擦锚点][contact-gather] · [摩擦锥裁剪][cone] · [摩擦迭代设置][iterations]

### 5.3 重建速度，再求反弹和动摩擦

位置迭代后，速度阶段先把实际用于速度重建的已累积位移除以 `Δt` 加入速度，角速度同理，分裂冲量修正可被扣除。接触相对速度取 `v₀ + ω₀×r₀ − v₁ − ω₁×r₁`。若初始撞击速度越过恢复阈值，目标法向速度为 `−e × v_initial_normal`，否则为 0。将 `uₙ` 记为当前相对法向速度，`u_target` 为目标速度，`jₙ` 为本步累计法向速度冲量。位置阶段已推开的量折算为 `j_pos` 后，标量速度修正可写为

~~~text
Δj_raw = −stiffness × M(n) × (u_n − u_target)
j_next = max(min(0, −j_pos), j_old + Δj_raw)
Δj     = j_next − j_old
~~~

其中 `j_pos = (λ_hard + λ_soft)/Δt`。这个下限允许速度阶段抵消位置修正造成的多余分离速度。切向动摩擦预算也以位置和速度的合计法向贡献为基础，并判断已有切向位置修正占了多少预算。 [重建隐式速度][implicit-phase] · [速度公式][implicit] · [恢复目标][contact-gather] · [冲量截断和摩擦][velocity] · [接触速度循环][velocity-loop]

### 5.4 关节的锚点与角度限制

点关节先把双方局部连接点转到世界空间，形成三维误差 `C = anchor₁−anchor₀`。两个动态体的标量 C++ 路径构造两侧 3×3 响应矩阵 `K₀,K₁`，各自包含逆质量和力臂引起的转动。用 `Δp = stiffness × (K₀+K₁)⁻¹ C` 得到修正，分别施加相反的线向与角向改变量。点关节两侧都能施力，所以这里没有接触法线的单侧截断。 [关节阶段][joint] · [双动态体点约束][joint-point]

角约束用双方连接坐标系求相对旋转。Twist 限位取绕关节轴的角度，仅修正超出 ±上限的部分，负侧超限时翻转修正轴。Swing 可从两个方向的上限构造椭圆锥，只修正偏出该锥的角度。软限制还使用刚度、阻尼和累计乘子。投影阶段用于支持它的约束类型，标量碰撞容器的投影接口为空。 [Twist 与 Swing 限位][joint-angular] · [接触容器的投影接口][projection]

## 6. 破碎怎样从一个刚体变成多个

Geometry Collection 在运行前提供碎片、局部变换、形状和碎片层级。连接图可由资产提供，也可按配置用形状近邻或 Delaunay 邻域建立，再按包围和连接条件筛选。未断开的父簇作为一个活动刚体。成员的质量与质心先换到父空间，质心是质量加权平均。惯量先旋转到同一坐标系，再按平行轴定理叠加到共同质心，即各子体贡献为 `Rᵢ Iᵢ Rᵢᵀ + mᵢ (‖rᵢ‖² E − rᵢrᵢᵀ)`。这里 `rᵢ` 是子质心相对父质心的偏移，`E` 是单位矩阵。最后对角化得到主惯量与惯量旋转。 [Geometry Collection 流程][destruction] · [连接图选项][connections] · [父空间质量聚合][cluster-mass] · [质心和惯量合并][mass-combine]

在一次主求解后，聚类阶段可将碰撞累计冲量转成作用于簇成员的损伤指标，也可接收场输入。`ReleaseClusterParticlesImpl` 对子体比较 `max(碰撞指标, 外部应变) ≥ 内部阈值`。满足条件的孩子从父节点释放，记录破碎事件。一种传播使用断裂后剩余应变，另一种按冲击因子传播未断裂的响应。断开后按配置清理连接并检查仍关联的子集，可以为仍相连的块生成新簇。该过程受每步破碎预算约束。阈值与冲量属性是资产和损伤模式的一部分。 [碰撞冲量累积路径][impact] · [释放条件][release] · [传播与后续连通性][propagate] · [每步阈值与预算入口][break-budget]

## 7. 历史回退与可观察结果

步末统一处理岛休眠、失活和新碎片，将脏粒子排入发布。`FRewindData` 按帧与阶段记录所需物理状态，检查目标帧与缓存范围。重模拟从历史帧恢复状态，应用校正并重播之后输入，因而还要保留对应的推送包。相比 CCD 的步内 TOI，网络重播依赖跨步历史和输入。Chaos Visual Debugger 则能记录碰撞、求解和其他阶段供回放检查。观察堆叠时可分别查看候选对、最终流形、岛大小、位置迭代、速度迭代和破碎输出，找到最先偏离预期的步骤。 [步末提交][finish] · [历史范围及恢复][rewind] · [网络重模拟][network] · [Chaos Visual Debugger][cvd]

[rigids]: https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos/FPBDRigidsSolver_1?application_version=5.8
[substep]: https://dev.epicgames.com/documentation/en-us/unreal-engine/physics-sub-stepping-in-unreal-engine?application_version=5.8
[destruction]: https://dev.epicgames.com/documentation/en-us/unreal-engine/destruction-overview?application_version=5.8
[network]: https://dev.epicgames.com/documentation/en-us/unreal-engine/networked-physics-overview?application_version=5.8
[cvd]: https://dev.epicgames.com/documentation/en-us/unreal-engine/chaos-visual-debugger-in-unreal-engine?application_version=5.8
[solver-mode]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L503-L527
[marshal]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/ChaosMarshallingManager.cpp#L42-L136
[push-apply]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L1974-L2180
[buffer]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PBDRigidsSolver.cpp#L2996-L3126
[pull]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/PhysicsProxy/SingleParticlePhysicsProxy.cpp#L272-L408
[advance]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L529-L723
[finish]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L729-L829
[integrate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L906-L1048
[kinematic]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L1080-L1264
[broad]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/SpatialAccelerationBroadPhase.cpp#L17-L153
[mid-bvh]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/ParticlePairMidPhase.cpp#L1308-L1452
[manifold]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/ParticlePairMidPhase.cpp#L471-L567
[swept-contact]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/ParticlePairMidPhase.cpp#L572-L682
[sphere-convex]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/SphereConvexContactPoint.cpp#L45-L115
[gjk]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/GJK.h#L2086-L2170
[epa]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/GJK.h#L1795-L1899
[ccd-mode]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/CCDUtilities.cpp#L100-L135
[ccd-order]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/CCDUtilities.cpp#L517-L624
[ccd-resweep]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/CCDUtilities.cpp#L640-L752
[advance-ccd]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidsEvolutionGBF.cpp#L655-L760
[island]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandManager.cpp#L1637-L1735
[island-updates]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandManager.cpp#L2065-L2268
[groups]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandGroupManager.cpp#L120-L218
[gather]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Island/IslandGroupManager.cpp#L493-L617
[group-solve]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Evolution/ConstraintGroupSolver.cpp#L422-L437
[contact-gather]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/PBDCollisionContainerSolver.cpp#L74-L177
[mass]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L781-L834
[position]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L452-L471
[pos-apply]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L593-L652
[cone]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L487-L534
[iterations]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/PBDCollisionContainerSolver.cpp#L837-L905
[implicit-phase]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Evolution/ConstraintGroupSolver.cpp#L465-L476
[implicit]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Evolution/SolverBody.h#L137-L150
[velocity]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L660-L737
[velocity-loop]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Public/Chaos/Collision/PBDCollisionSolver.h#L1151-L1242
[joint]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Joint/PBDJointSolverGaussSeidel.cpp#L221-L283
[joint-point]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Joint/PBDJointSolverGaussSeidel.cpp#L1973-L2019
[joint-angular]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Joint/PBDJointSolverGaussSeidel.cpp#L1595-L1682
[projection]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/Collision/PBDCollisionContainerSolver.cpp#L734-L767
[connections]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L2113-L2168
[cluster-mass]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClusteringAlgo.cpp#L46-L104
[mass-combine]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/MassProperties.cpp#L274-L345
[impact]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L2251-L2381
[release]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L1165-L1247
[propagate]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L1257-L1427
[break-budget]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/Chaos/PBDRigidClustering.cpp#L1656-L1750
[rewind]: https://github.com/EpicGames/UnrealEngine/blob/f97cae4acf019789ed431e0ac28d47e15422784c/Engine/Source/Runtime/Experimental/Chaos/Private/RewindData.cpp#L901-L1072
