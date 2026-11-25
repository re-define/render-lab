# 银狼的动作、接触和物理

在 `silver-contact` 中，路线依次经过断槽、矮栏、低横梁、斜坡、弹簧踏板和终点按钮。普通步行从 39 段重定向动作中检索；跳跃、撑栏和钻行各使用一段单独生成的 Kimodo 片段。角色质量为 48 kg，固定子步为 1/60 秒。[场景装配](../src/experiments/motion/silver/contact.rs) · [障碍几何](../src/experiments/motion/silver/contact/course.rs)

## 一个子步

1. **意图和基础姿态。** 恢复上一步 IK 之前的局部姿态。控制器依据当前速度、加速度和角速度预测弯曲的目标轨迹。匹配器组合未来轨迹与脚部特征；查询与训练库采用同一套中心和缩放，使用预构建的 PCA / KD 索引或扫描参考路径。切换片段时以惯性化衰减姿态差，保持骨骼连续。多个角色可以在同一系统内并行采样各自的姿态。[动作匹配](../src/experiments/motion/silver.rs) · [轨迹积分](../src/experiments/motion/silver/locomotion.rs) · [查询](../src/experiments/motion/silver/search.rs)
2. **动作通知。** 起跳标记在片段的前次采样与本次采样之间检测，跨过标记触发一次。保持在同一帧或重复采样不会重复施加冲量。撑手是持续窗口，定义接近、完全支撑和释放的源帧。动作时钟及触地状态随场景保存。[事件表](../src/experiments/motion/silver/contact/events.rs) · [动作播放](../src/experiments/motion/silver/contact/traversal.rs)
3. **物理与落点预测。** 地面牵引受限，蹬地时施加竖直冲量，空中用恒加速度积分线动量和位置。每个可行走支撑面有有限边界。跳跃预测解重力抛物线与移动平面的下降交点，并检查落点是否位于该面；最早有效交点用于选择空中动作相位。下一次连续碰撞检测决定实际落地。踏板有 22 kg 的竖直自由度、弹簧与阻尼，承受接触反冲。对平移的支撑面，使用其速度计算相对碰撞速度和地面移动，离地继承载体速度。[动力学](../src/experiments/motion/silver/contact/dynamics.rs) · [冲量求解](../src/physics.rs)
4. **足底与手部反馈。** 脚底锚点存在支撑面局部坐标。抬脚、拉伸过大或换支撑面时先渐出再重新捕获。膝盖沿源动画弯曲平面求解，近伸直时向角色前方偏置。翻越时掌根对齐栏面，实际掌根误差影响下一子步承重。到终点由末端手指的蒙皮点反推腕部目标。最终局部变换通过标准提取和 GPU 蒙皮进入画面。[反馈阶段](../src/experiments/motion/silver/contact/stage.rs) · [双骨 IK](../src/experiments/motion/silver/contact/rig.rs)

## 验证和可调参数

| 命令 | 观察 |
| --- | --- |
| `contact action jump`、`contact action vault`、`contact action duck` | 在对应入口和固定机位单独播放一次 |
| `contact gravity 7`、`contact impulse 160` | 改重力或下一次竖直冲量，用同一片段比较最高点和飞行时间 |
| `contact physics`、`contact status` | 查看速度、动量、落地及撑手冲量、着地脚数、触摸误差 |
| `contact feet off`、`contact hand off` | 单独观察足部或手部修正的贡献，用 `on` 恢复 |
| `contact manual`、`contact auto`、`contact view hand` | 切换手动行走、自动路线和终点特写 |

同质量同片段，默认重力 9.81 m/s²、冲量 137.34 N·s 时根节点上升 0.417 m、腾空 0.583 s；把重力改为 7 后为 0.585 m、0.818 s。移动平台测试还验证了每秒 0.60 m 的横向平台运动在起跳后得到继承。当前实验模拟平移质点、有限顶面接触和单自由度踏板，转身和肢体姿态由动画与 IK 控制。旋转载体、布娃娃、多刚体关节及联网回滚不在当前实现范围。[回归与净空测量](../src/experiments/motion/silver/contact/tests.rs) · [障碍场实机录像](../captures/silver-contact.mp4)

## 与 Unreal 的对应关系

截至 2026 年 11 月 25 日，Epic 已公布 UE 5.9 Preview 2。5.9 文档列出 Unreal Animation Framework 的通知和分层扩展；Mover 公开说明将运动模式、时间步和物理反馈分开。本实验采用动作事件窗口、运动支撑速度和实际着地反馈，在本引擎自己的调度器内实现。Kimodo 在本项目离线生成动作，运行时加载烘焙数据。[Epic 预览公告](https://forums.unrealengine.com/t/preview-2-unreal-engine-5-9-preview/2852840) · [5.9 动画说明](https://dev.epicgames.com/documentation/unreal-engine/unreal-engine-5-9-release-notes) · [Mover](https://dev.epicgames.com/documentation/unreal-engine/mover-in-unreal-engine) · [Chaos 地面约束](https://dev.epicgames.com/documentation/unreal-engine/API/Runtime/Chaos/FCharacterGroundConstraint/SetTargetDeltaPosition)

生成和重定向分别由 [Kimodo 动作生成脚本](../tools/generate_kimodo.py)、[银狼烘焙脚本](../tools/bake_silver_wolf.py)、[检索索引脚本](../tools/motion_index.py) 与 [障碍动作生成脚本](../tools/generate_silver_traversal.py) 负责。
