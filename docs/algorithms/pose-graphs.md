# 银狼的姿态图

在 `silver-contact` 或 `silver-contact-duo` 中，每个角色可以选择自己的姿态图。基础姿态来自 Motion Matching 或正在执行的穿越动作。图决定在哪些骨骼上混入另一段动作，以及物理骨链、落脚和手部 IK 按什么顺序修改最终姿态。默认图在骨链之后先解脚，再解手。第二份预设在骨链之前加入跑步片段的胸、肩和双臂旋转。[默认图](../../assets/motion/pose-graphs/default.json) · [上半身图](../../assets/motion/pose-graphs/upper-body.json)

## 从图资产到一次求值

1. 每个节点有名字，`output` 指向输出节点。编译器从输出沿 `input` 回溯到 `input` 类型的基础姿态节点，再生成正向执行序列。`masked_blend.overlay` 单独引用一个 `clip` 节点，多个层可以读取同一个片段。编译时解析片段区间和通道索引，检查环、不可达节点、未知片段或骨骼、越界权重和时间窗，以及同一有状态算子被用两次的情况。全零遮罩不产生求值指令。[图定义](../../src/animation.rs) · [编译器](../../src/animation/program.rs)
2. 角色组件保存图资产的实体 ID。相同资产编译成一份只读指令序列，由绑定到它的角色共享。角色各自保存图的时间、脚锁、触达权重和骨链状态。图的修改触发重新绑定。存档保存图本身和角色状态，重载时可以从当前播放相位继续。早期存档没有图字段，载入时选择默认图。[资产与播放绑定](../../src/experiments/motion/silver/contact/pose_graph.rs) · [角色组件](../../src/experiments/motion/silver/contact.rs)
3. 每个 60 Hz 角色子步先采样基础动作，提交期望根运动，等待物理给出实际根位姿、重力和附近支撑面。姿态阶段先在基础动作上评估机械握持，再按图顺序运行修改器。每个并行作业复用局部和世界矩阵缓冲，骨架在角色内按父节点先行求值。工作线程将结果登记为一次姿态发布，供场景提取和 GPU 蒙皮消费。[角色子步](../architecture/character-motion.md) · [姿态求值](../../src/experiments/motion/silver/contact/stage.rs)
4. `clip` 使用动作库的 30 Hz 采样率。从角色图时间乘以片段速度得到循环相位，两帧之间用四元数球面插值，末帧与首帧相接。每个选中通道先经角色的绑定基变换，再与基础局部旋转插值。权重等于层权重、通道权重和时间包络三者之积。局部平移和缩放沿用基础姿态，绑定时按骨架拓扑求出遮罩骨骼及其后代的并集。相邻的局部混合按原顺序执行，只在这一组结束时按父节点先行刷新并集内的世界矩阵。IK 或物理骨链是分界点，读取完整的当前姿态。若一组混合全部处于时间窗外，直接跳过这次刷新。[采样与混合](../../src/experiments/motion/silver/contact/pose_graph.rs)

`masked_blend` 可指定以秒为单位的 `"window": [0.2, 0.4, 0.9, 1.1]`。从图启用后的 0.2 秒开始平滑淡入，0.4 至 0.9 秒保持完整权重，1.1 秒淡出结束。窗口以图启用为起点，片段按自身周期循环。省略窗口时一直使用配置的权重。切换到另一份图会重置图时钟。

节点顺序会改变结果。要保持指尖对准目标，让手部 IK 位于影响手腕的层之后。把混合移到手部 IK 之后会覆盖一部分求解旋转。去掉落脚节点还会清除足部锚点，下一子步的 Motion Matching 会由此获得不同的落脚观察。两份内置图都保留落脚节点。[节点顺序与回归](../../src/experiments/motion/silver/contact/pose_graph/tests.rs)

同一 Rayon 作业在采样片段时，用动作库中的两个实际帧索引和精确的小数相位作键，按需复用已经插值的源通道。缓存只包括源旋转，角色自己的绑定基、当前基础姿态和遮罩权重仍分别计算。相位不同时重新采样。角色按连续小批分给线程，每批暂存编辑，提交时保持输入顺序，失败的批组丢弃全部编辑。[采样实现](../../src/experiments/motion/silver/contact/pose_graph.rs) · [矩阵等价回归与内核基准](../../src/experiments/motion/silver/contact/rig/evaluation_tests.rs)

## 运行与验证

[银狼撑栏并排录像](../../captures/silver-pose-graph.mp4)使用同一输入和机位，左边为默认图，右边为九通道上半身图。每路 640×480，TAA，30 FPS，共 78 帧。另一次关闭后处理的静帧对照中，旧版与默认图的 307,200 个 RGB 像素完全一致；默认图与上半身图有 6,599 个像素发生变化。采集命令如下。

```powershell
cargo run --release --locked -- --experiment silver-contact --command "contact action vault" --command "contact graph upper" --command "post off" --size 640x480 --warmup 16 --capture target/vault-upper.png
```

```powershell
cargo run --release --locked -- --experiment silver-contact --command "contact action vault" --command "contact graph upper"
```

控制台输入 `contact graph` 查看当前角色的图、节点数和播放时间。`contact graph default` 恢复默认图。`contact graph load assets/motion/pose-graphs/upper-body.json` 读取自定义 JSON，校验成功后切换。双人场景先用 `control ID` 选角色，可让两个角色使用同一资产并在不同时刻开始播放。[实验目录](../experiment-catalog.md)

```powershell
cargo test --release --locked --lib pose_graph
```

回归覆盖撑栏根轨迹、机械承重、上下身局部旋转、不同工作线程数下的存档续播、两种骨架绑定共享图、无效资产的原子拒绝，以及改变图连线后末端姿态的变化。可从性能文档中复现默认图和九通道上半身图的人群耗时。[性能与复现](../architecture/performance.md)

本实现支持一条可变基础姿态及多个只读片段输入。动作检索、惯性化和根运动仍由银狼角色系统提供。引擎对新算子的扩展入口在 Rust 枚举和求值器，独立 `motion` 实验仍直接运行选姿与采样。[与 Unreal Animation Framework 的对照](../unreal/animation-physics.md)
