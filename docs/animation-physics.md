# 银狼的动作、接触和物理

在 `silver-contact` 中，路线依次经过断槽、矮栏、低横梁、斜坡、弹簧踏板和终点按钮。普通步行从 39 段重定向动作中检索；跳跃、撑栏和钻行各使用一段单独生成的 Kimodo 片段。角色质量为 48 kg，固定子步为 1/60 秒。[场景装配](../src/experiments/motion/silver/contact.rs) · [障碍几何](../src/experiments/motion/silver/contact/course.rs)

## 一个子步

1. **意图与基础姿态。** 控制器根据输入、速度、加速度和角速度预测轨迹。匹配器用未来轨迹和足部特征查询动作库。训练样本与查询使用同一套标准化参数，可使用 PCA / KD 索引或扫描。片段切换时，惯性化读取角色保留的上一份基础姿态。匹配器将本步采样的 21 个局部变换直接交给接触阶段，同时发布角色根节点的运动目标。独立的银狼移动实验可直接发布采样姿态。[姿态采样](../src/experiments/motion/silver.rs) · [轨迹预测](../src/experiments/motion/silver/locomotion.rs) · [动作检索](../src/experiments/motion/silver/search.rs)
2. **动作通知。** 越过起跳标记会施加一次冲量，保持在同一采样时刻不会重复起跳。撑手动作定义接近、支撑和释放三个区间。动作时钟、起跳和触地状态都会保存。[动作事件](../src/experiments/motion/silver/contact/events.rs) · [片段与动作对齐](../src/experiments/motion/silver/contact/traversal.rs)
3. **动力学与落点。** 角色在地面受限于牵引力，腾空时按恒定重力积分线动量和位置。角色预测重力抛物线与移动顶面的首次下降交点，排除落在顶面边界之外的候选点，用预计剩余时间调整空中片段。连续碰撞检测确认实际着地。支撑面平移时，角色随支撑面移动，离地时继承其速度。法线、边界和相对运动的查询由物理模块提供。[角色积分](../src/experiments/motion/silver/contact/dynamics.rs) · [质点冲量](../src/physics.rs) · [有限支撑面](../src/physics/support.rs)
4. **接触与最终姿态。** IK 读取本步物理求解后的支撑面、角色根节点和基础姿态。脚底锁定在支撑面的局部坐标，转移支撑时渐出。膝盖沿源动画的弯曲平面求解，接近伸直时增加前向偏置。撑栏时校正掌根，上一子步的撑手质量参与本步承重。触摸按钮时根据手指的绑定空间触点反求腕部目标。每次肢体校正只更新受影响的骨骼子树，最终局部骨骼姿态写入 ECS 一次，供场景提取和 GPU 蒙皮使用。[接触阶段](../src/experiments/motion/silver/contact/stage.rs) · [FK 与双骨 IK](../src/experiments/motion/silver/contact/rig.rs)

## 多角色、共享支撑与存档

装配器枚举具有接触组件的角色。每个角色的腿、髋、手臂取自动作库约定的关节槽位，绑定时检查关节是否属于该角色。指尖触点可指定右腕子树内的关节 ID 和局部偏移。银狼默认在自己的腕部子树内解析食指末端。角色和可交互装置使用共同的父坐标系。普通预设有一名角色，多人压测和双人碰撞测试都运行相同的装配和调度代码。[角色绑定](../src/experiments/motion/silver/contact.rs) · [多人场景构造](../src/experiments/motion/silver/contact/crowd_profile.rs)

物理系统按弹簧踏板 ID 为角色分组。不同踏板可并行求解。一个踏板在子步开始时预测一次位移和速度，组内每个角色据此计算起跳或着地冲量。求解器按角色 ID 顺序累加平台的动量和位移增量，限制行程，提交一次踏板状态，再校正其上所有角色的根节点。弹簧速度保存在踏板上。加载旧场景时会从原角色组件迁移该速度。[物理分组与归约](../src/experiments/motion/silver/contact/dynamics.rs) · [载入迁移](../src/experiments/motion/silver/contact.rs)

存档包含上一份基础姿态、动作相位、足部锚点、物理状态，以及生成该姿态时使用的骨骼和装置绑定。固定子步会检测成员和绑定变化。替换头骨、指尖、目标或支撑装置后，角色重新绑定，清除旧的基础姿态、运动过渡以及足部接触历史。读取旧版无绑定信息的存档时接受原有基础姿态，第一个成功子步会记录当前绑定。支撑面查询缓存在物理阶段重建。[存档与热替换测试](../src/experiments/motion/silver/contact/tests.rs)

## 验证和可调参数

| 命令 | 观察 |
| --- | --- |
| `contact action jump`、`contact action vault`、`contact action duck` | 在对应入口和固定机位单独播放一次 |
| `contact gravity 7`、`contact impulse 160` | 改重力或下一次竖直冲量，用同一片段比较最高点和飞行时间 |
| `contact physics`、`contact status` | 查看速度、动量、落地及撑手冲量、着地脚数、触摸误差 |
| `contact feet off`、`contact hand off` | 单独观察足部或手部修正的贡献，用 `on` 恢复 |
| `contact manual`、`contact auto`、`contact view hand` | 切换手动行走、自动路线和终点特写 |

以下为 60 Hz 固定子步回归结果。距离以米为模拟单位，表内接触误差换算为毫米。

| 验证 | 结果与测量口径 |
| --- | --- |
| 关节校正开销 | 同一姿态依次校正双腿和单臂，三次求解共更新 33 个 FK 矩阵。每次旋转都更新全部关节时为 279 个。校正后的每个矩阵均与全量 FK 逐元素相同。 |
| 终点触摸 | 在 1,080 步路线的最后 120 步，测量完全绑定到指尖的蒙皮顶点与按钮目标的世界空间距离，P95 为 0.0107 mm。 |
| 翻栏和站稳 | 撑栏完整接触窗口内掌根到栏面目标的最大距离为 0.0014 mm。第 841 步之后，角色空间中膝、踝、脚趾相邻子步位移的最大值为 0.143 mm。 |
| 弹道与平台 | 同一片段、48 kg 质量和 137.34 N·s 冲量下，重力 9.81 m/s² 时顶点升高 0.417 m，腾空 0.583 s。改为 7 m/s² 后为 0.585 m 和 0.818 s。横向平台速度 0.60 m/s 在起跳时得到继承。 |
| 双人共享踏板 | 相同初态下，两名角色产生的首步踏板速度变化是单人的两倍。给第二个角色的骨骼改名，显式设置指尖，分别用一与四个工作线程运行，时间步一致时踏板变换相同、各关节在测试容差内重合。 |
| 保存与重绑 | 保存撑栏中的世界后，分别以每帧一个和两个固定子步推进，相同模拟时刻比较角色动量、撑手冲量、踏板与关节姿态。另一项测试在运行中替换头骨，保存尚未重新求值的姿态，确认在线和重载路径都丢弃旧矩阵。 |

角色根节点采用平移动力学，步态和肢体方向由片段与 IK 决定。当前覆盖有限支撑面、平移载体与竖直单自由度弹簧，旋转支撑面的角速度、多刚体布娃娃、任意拓扑角色自动重定向和网络回滚有待验证或实现。双人用例覆盖同一骨架结构的异名绑定。[完整路线与热替换](../src/experiments/motion/silver/contact/tests.rs) · [子树 FK 对照](../src/experiments/motion/silver/contact/rig.rs) · [双人共享踏板](../src/experiments/motion/silver/contact/crowd_profile.rs) · [障碍场录像](../captures/silver-contact.mp4)

## 与 Unreal 的对应关系

截至 2026 年 11 月 25 日，Epic 已公布 UE 5.9 Preview 2。5.9 文档列出 Unreal Animation Framework 的通知和分层扩展；Mover 公开说明将运动模式、时间步和物理反馈分开。本实验采用动作事件窗口、运动支撑速度和实际着地反馈，在本引擎自己的调度器内实现。Kimodo 在本项目离线生成动作，运行时加载烘焙数据。[Epic 预览公告](https://forums.unrealengine.com/t/preview-2-unreal-engine-5-9-preview/2852840) · [5.9 动画说明](https://dev.epicgames.com/documentation/unreal-engine/unreal-engine-5-9-release-notes) · [Mover](https://dev.epicgames.com/documentation/unreal-engine/mover-in-unreal-engine) · [Chaos 地面约束](https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos/FCharacterGroundConstraint/SetTargetDeltaPosition)

生成和重定向分别由 [Kimodo 动作生成脚本](../tools/generate_kimodo.py)、[银狼烘焙脚本](../tools/bake_silver_wolf.py)、[检索索引脚本](../tools/motion_index.py) 与 [障碍动作生成脚本](../tools/generate_silver_traversal.py) 负责。
