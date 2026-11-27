# 银狼的动作、接触和物理

在 `silver-contact` 中，路线依次经过断槽、矮栏、低横梁、斜坡、弹簧踏板和终点按钮。普通步行从 39 段重定向动作中检索；跳跃、撑栏和钻行各使用一段单独生成的 Kimodo 片段。角色质量为 48 kg，固定子步为 1/60 秒。[场景装配](../src/experiments/motion/silver/contact.rs) · [障碍几何](../src/experiments/motion/silver/contact/course.rs)

## 一个子步

`character` 时钟每秒运行 60 个固定子步，每次追帧最多执行 8 步。动作采样、载体旋转、角色动力学和接触 IK 分别注册到同名时钟。每个子步先完成该步的基础姿态和支撑变换，再求解动力学，最后运行 IK。注册顺序不影响依赖，角色绑定可共用一次计算。任一阶段出错会撤销该子步中已提交的写入，之前成功的子步保留。其他命名时钟或独立固定阶段可选择自己的步长和追帧上限。[时钟与系统注册](../src/engine/schedule.rs) · [子步重试与多线程验证](../tests/simulation_schedule.rs)

1. **意图与基础姿态。** 控制器根据输入、速度、加速度和角速度预测轨迹。匹配器用未来轨迹和足部特征查询动作库。训练样本与查询使用同一套标准化参数，可使用 PCA / KD 索引或扫描。片段切换时，惯性化读取角色保留的上一份基础姿态。匹配器将本步采样的 21 个局部变换直接交给接触阶段，同时发布角色根节点的运动目标。独立的银狼移动实验可直接发布采样姿态。[姿态采样](../src/experiments/motion/silver.rs) · [轨迹预测](../src/experiments/motion/silver/locomotion.rs) · [动作检索](../src/experiments/motion/silver/search.rs)
2. **动作通知。** 越过起跳标记会施加一次冲量，保持在同一采样时刻不会重复起跳。撑手动作定义接近、支撑和释放三个区间。动作时钟、起跳和触地状态都会保存。[动作事件](../src/experiments/motion/silver/contact/events.rs) · [片段与动作对齐](../src/experiments/motion/silver/contact/traversal.rs)
3. **动力学与落点。** 角色在地面受限于牵引力，腾空时按恒定重力积分线动量和位置。角色预测重力抛物线与移动顶面的首次下降交点，排除落在顶面边界之外的候选点，用预计剩余时间调整空中片段。连续碰撞检测确认实际着地。支撑点随载体做刚体运动。一般支撑面用相邻姿态估计刚体运动；转台直接提供有符号的驱动角速度，以盒体原点为旋转中心。接触点速度为 `v_pivot + ω × (p − pivot)`。即使采样间隔跨过整圈，转台也能保留真实转向和速度。角色起跳时继承支撑点速度。平移顶面用解析交点，角速度非零时在三秒预测范围内查找下降穿越并二分细化。法线、边界和相对运动的查询由物理模块提供。[角色积分](../src/experiments/motion/silver/contact/dynamics.rs) · [质点冲量](../src/physics.rs) · [有限支撑面](../src/physics/support.rs)
4. **接触与最终姿态。** IK 读取本步物理求解后的支撑面、角色根节点和基础姿态。脚底锁定在支撑面的局部坐标，转移支撑时渐出。膝盖沿源动画的弯曲平面求解，接近伸直时增加前向偏置。撑栏时校正掌根，上一子步的撑手质量参与本步承重。触摸按钮时根据手指的绑定空间触点反求腕部目标。每次肢体校正只更新受影响的骨骼子树，最终局部骨骼姿态写入 ECS 一次，供场景提取和 GPU 蒙皮使用。[接触阶段](../src/experiments/motion/silver/contact/stage.rs) · [FK 与双骨 IK](../src/experiments/motion/silver/contact/rig.rs)

## 角色根运动的所有权

键盘和回放把移动意图发给选中的实体。动画采样产生角色父空间中的目标姿态，`RootMotionRequest` 交给运动模块解析。静止角色可继续播放骨骼动作且根矩阵保持原值；运动学角色采用动画目标；动态角色采用目标朝向和物理解算后的位置。接触版银狼从本子步的提议计算牵引与冲量，平台解决所有乘员后只提交一次角色根，随后 IK 读最终位置、支撑面和接触信息。请求在子步内消费，恢复存档后的首个子步重新生成。[移动契约](../src/movement.rs) · [动作采样与 IK](../src/experiments/motion/silver/contact/stage.rs) · [支撑求解](../src/experiments/motion/silver/contact/dynamics.rs)

银狼独立动作实验默认由运动学驱动；选择动态根需要接触动力学提供者。接触实验的角色与弹簧板默认参与积分，起跳板是可动的查询表面，台阶是固定查询表面。关掉支撑的物理参与会把它从当步接触查询中移除。角色只查询时仍可得到脚底支撑，关闭查询会释放足部锚点。重力跳跃与撑栏翻越使用动态角色；运动学控制可用于无碰撞行走。缺少新策略字段的旧存档继续采用其原实验的行为。[策略定义](../src/scene/motion_policy.rs) · [场景与回归](../src/experiments/motion/silver/contact/tests.rs)

### 500 人的移动与提取成本

在同一台机器上交替运行重构前的 `4b6ed7c` 和引入运动策略、模块时钟后的 release 版本。场景包含 177,042 个实体、500 名角色、3,000 个蒙皮实例。每次预热 20 帧、采样 96 帧，使用 16 个 worker。每帧推进 0.1 秒模拟时间，合计六个 1/60 秒子步，再做一次场景提取。测试仅运行 CPU 路径，采样首帧与末帧还逐实例核对增量与全量提取的变换和骨骼调色板。[压测与一致性检查](../src/experiments/motion/silver/contact/crowd_profile.rs)

| 运行顺序 | 版本 | 模拟 P50 / P95（ms） | 提取 P50 / P95（ms） |
| --- | --- | ---: | ---: |
| 1 | 重构后 | 24.73 / 27.02 | 7.50 / 8.34 |
| 2 | 重构前 | 27.69 / 37.59 | 7.61 / 9.54 |
| 3 | 重构后 | 26.53 / 32.96 | 7.61 / 8.80 |
| 4 | 重构前 | 24.66 / 26.76 | 7.20 / 7.68 |
| 最终复验 | 混合角色按实体归属，转台按驱动角速度查询 | 25.12 / 28.95 | 7.51 / 8.22 |

每帧四次测试都读取 500 个姿态块、10,500 个变化关节和 159,500 个调色板关节。重构前后各项耗时范围均有重叠，两个版本的快慢在两轮对比中相反。复测时可将环境变量 `RENDER_LAB_NPCS` 设为 `500`，运行 `cargo test --release --lib silver_crowd_extraction_profile -- --ignored --nocapture --test-threads=1`。

## 多角色、共享支撑与存档

装配器枚举具有接触组件的角色。每个角色的腿、髋、手臂取自动作库约定的关节槽位，绑定时检查关节是否属于该角色。指尖触点可指定右腕子树内的关节 ID 和局部偏移。银狼默认在自己的腕部子树内解析食指末端。角色和可交互装置使用共同的父坐标系。`--experiment silver-contact-duo` 可以直接打开双人障碍场。两个角色共用网格，第二个角色的腕部层次和长度不同。输入 `control` 列出实体，`control ENTITY` 指定接收 IJKL 和接触命令的角色。多人压测和双人碰撞测试都运行相同的装配和调度代码。[角色绑定](../src/experiments/motion/silver/contact.rs) · [多人场景构造](../src/experiments/motion/silver/contact/crowd_profile.rs)

物理系统按弹簧踏板 ID 为角色分组。不同踏板可并行求解。一个踏板在子步开始时预测一次位移和速度，组内每个角色据此计算起跳或着地冲量。求解器按角色 ID 顺序累加平台的动量和位移增量，限制行程，提交一次踏板状态，再校正其上所有角色的根节点。弹簧速度保存在踏板上。加载旧场景时会从原角色组件迁移该速度。[物理分组与归约](../src/experiments/motion/silver/contact/dynamics.rs) · [载入迁移](../src/experiments/motion/silver/contact.rs)

存档包含上一份基础姿态、动作相位、足部锚点、物理状态，以及生成该姿态时使用的骨骼和装置绑定。固定子步会检测成员和绑定变化。替换头骨、指尖、目标或支撑装置后，角色重新绑定，清除旧的基础姿态、运动过渡以及足部接触历史。读取旧版无绑定信息的存档时接受原有基础姿态，第一个成功子步会记录当前绑定。支撑面查询缓存在物理阶段重建。[存档与热替换测试](../src/experiments/motion/silver/contact/tests.rs)

## 验证和可调参数

| 命令 | 观察 |
| --- | --- |
| `contact action jump`、`contact action vault`、`contact action duck` | 在对应入口和固定机位单独播放一次 |
| `contact gravity 7`、`contact impulse 160` | 改重力或下一次竖直冲量，用同一片段比较最高点和飞行时间 |
| `contact physics`、`contact status` | 查看速度、动量、落地及撑手冲量、着地脚数、触摸误差 |
| `contact feet off`、`contact hand off` | 单独观察足部或手部修正的贡献，用 `on` 恢复 |
| `control`、`control ENTITY` | 查询或切换 IJKL 输入所属的角色；切换会释放上一角色的键盘运动。 |
| `contact spin 45` | 将起跳台角速度设为每秒 45 度，零速可停止旋转。 |
| `contact manual`、`contact auto`、`contact view hand` | 切换手动行走、自动路线和终点特写 |

以下为 60 Hz 固定子步回归结果。距离以米为模拟单位，表内接触误差换算为毫米。

| 验证 | 结果与测量口径 |
| --- | --- |
| 关节校正开销 | 同一姿态依次校正双腿和单臂，三次求解共更新 33 个 FK 矩阵。每次旋转都更新全部关节时为 279 个。校正后的每个矩阵均与全量 FK 逐元素相同。 |
| 终点触摸 | 在 1,080 步路线的最后 120 步，测量完全绑定到指尖的蒙皮顶点与按钮目标的世界空间距离，P95 为 0.0107 mm。 |
| 翻栏和站稳 | 撑栏完整接触窗口内掌根到栏面目标的最大距离为 0.0014 mm。第 841 步之后，角色空间中膝、踝、脚趾相邻子步位移的最大值为 0.143 mm。 |
| 弹道与平台 | 同一片段、48 kg 质量和 137.34 N·s 冲量下，重力 9.81 m/s² 时顶点升高 0.417 m，腾空 0.583 s。改为 7 m/s² 后为 0.585 m 和 0.818 s。横向平台速度 0.60 m/s 在起跳时得到继承。 |
| 双人共享踏板 | 相同初态下，两名角色产生的首步踏板速度变化是单人的两倍。给第二个角色的骨骼改名，在腕部链条插入额外父关节，显式设置指尖，分别用一与四个工作线程运行，时间步一致时踏板变换相同、各关节在测试容差内重合。 |
| 保存与重绑 | 保存撑栏中的世界后，分别以每帧一个和两个固定子步推进，相同模拟时刻比较角色动量、撑手冲量、踏板与关节姿态。另一项测试在运行中替换头骨，保存尚未重新求值的姿态，确认在线和重载路径都丢弃旧矩阵。 |

角色根节点采用平移动力学，步态和肢体方向由片段与 IK 决定。当前覆盖有限支撑面、刚体载体和竖直单自由度弹簧。旋转载体用例以 0.8 rad/s 验证偏离转轴的接触点处的起跳切向速度，并对照不同工作线程数、每帧子步数和存档恢复。双人用例采用不同腕部层次和显式关节绑定。多刚体布娃娃、任意拓扑角色自动重定向和网络回滚仍待实现。[完整路线与热替换](../src/experiments/motion/silver/contact/tests.rs) · [子树 FK 对照](../src/experiments/motion/silver/contact/rig.rs) · [双人共享踏板](../src/experiments/motion/silver/contact/crowd_profile.rs) · [障碍场录像](../captures/silver-contact.mp4)

## 与 Unreal 的对应关系

截至 2026 年 11 月 25 日，Epic 已公布 UE 5.9 Preview 2。5.9 文档列出 Unreal Animation Framework 的通知和分层扩展；Mover 公开说明将运动模式、时间步和物理反馈分开。本实验采用动作事件窗口、运动支撑速度和实际着地反馈，在本引擎自己的调度器内实现。Kimodo 在本项目离线生成动作，运行时加载烘焙数据。[Epic 预览公告](https://forums.unrealengine.com/t/preview-2-unreal-engine-5-9-preview/2852840) · [5.9 动画说明](https://dev.epicgames.com/documentation/unreal-engine/unreal-engine-5-9-release-notes) · [Mover](https://dev.epicgames.com/documentation/unreal-engine/mover-in-unreal-engine) · [Chaos 地面约束](https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos/FCharacterGroundConstraint/SetTargetDeltaPosition)

生成和重定向分别由 [Kimodo 动作生成脚本](../tools/generate_kimodo.py)、[银狼烘焙脚本](../tools/bake_silver_wolf.py)、[检索索引脚本](../tools/motion_index.py) 与 [障碍动作生成脚本](../tools/generate_silver_traversal.py) 负责。


## 动作查询索引的增量烘焙

银狼动作库有 39 段、4,680 帧。每段末尾留出 23 帧预测窗口，总共得到 3,783 个可查询姿态。离线工具为 24 维特征拟合统一的均值、尺度和权重，投影到六个主成分后建立 511 个节点的 KD 树。运行时沿投影空间包围盒剪枝，以原始 24 维归一化距离和转场代价为候选打分。完整扫描与 KD 路径共用打分函数。[离线构建](../tools/motion_index.py) · [查询与运行时验证](../src/experiments/motion/silver/search.rs)

从仓库根目录运行以下命令。Python 环境需安装 NumPy。

```shell
python tools/motion_index.py
python tools/motion_index.py --check
python tools/motion_index.py --force
python -m unittest discover -s tools -p test_motion_index.py
```

第一条命令检查格式版本、NumPy 版本、烘焙脚本、动作清单和姿态数据的内容摘要，并校验树的原始字节。输入和索引一致时保留原文件与修改时间。第二条命令仅校验，陈旧或损坏时返回非零退出码。第三条命令强制重算。工具在同目录写临时文件，全部检查和拟合完成后原子替换产物。银狼重定向工具在写出动作清单和姿态后调用同一构建入口。运行时用内嵌的清单、姿态与烘焙配方校验版本和树内容，并检查叶子覆盖、节点边界。Python 测试覆盖缓存命中、确定性重建、同字节长度的输入变化、损坏恢复和单候选姿态。[烘焙回归](../tools/test_motion_index.py) · [银狼重定向入口](../tools/bake_silver_wolf.py)

本机对同一份银狼资产在临时目录各执行 20 次。已启动 Python 进程内，增量校验 P50 为 1.83 ms，强制烘焙 P50 为 14.39 ms。新产物与此前索引的均值、尺度、特征轴、节点边界和候选顺序一致，保留方差比例的浮点差小于 10⁻¹⁵。
