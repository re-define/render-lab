# 毛发动力学实验

## 调研与方案

| 方法 | 数据怎样流动 | 对本实验的启发 |
| --- | --- | --- |
| AMD TressFX | 模拟着色器把积分、形状保持、长度投影、碰撞和跟随发丝更新分成阶段。快速运动时可向发梢传递部分根运动，减轻瞬间拉长。 | 保留独立的静止造型、发丝历史位置和显示插值。用侧风、启停和头部摆动作可分离的输入。 [TressFX 开发指南](https://gpuopen.com/download/TressFX5.0_DeveloperGuide.pdf) · [模拟源码](https://github.com/GPUOpen-Effects/TressFX/blob/master/src/Shaders/TressFXSimulation.hlsl) |
| Dynamic Follow The Leader | 从固定根向梢单向逐段恢复长度，在速度更新中补偿位置投影引入的偏差。它以较低迭代成本满足不可伸长性，方向性和阻尼会影响动态响应。 | 将它保留为后续对照。当前先采用双向分配位置修正的粒子链，方便同时观测长度和接触误差。 [Müller 等，Fast Simulation of Inextensible Hair and Fur](https://matthias-research.github.io/pages/publications/FTLHairFur.pdf) |

第一版使用程序生成的假发和头模。头球、肩部胶囊及静止造型均由场景函数提供，长度单位为米。在同一机位比较无风、侧风和头部摆动，优先验证固定发根、发梢惯性、段长误差、碰撞余量和存档续算。显示阶段复用已有 GPU 蒙皮、可见性缓冲和材质着色。

## 从发根到屏幕

1. **造型与输入。** 64 条引导束环绕头皮，每束 14 点。前两点固定到头部，其余 12 点受力。CPU 在 120 Hz 固定时钟上记录侧风、刚度和头部运动的变化，并更新头模变换。相邻输入相同时共用一条记录。每个要补算的子步向 GPU 提供一个头部矩阵和四个标量，共 80 字节。[实验模型](../../src/experiments/hair/model.rs) · [输入历史](../../src/experiments/hair/timeline.rs)
2. **GPU 求解。** RDG 的 `Groom XPBD` compute pass 读取静止曲线与上一子步的两组粒子位置。一个 invocation 独占一束，各束并行，束内沿曲线顺序求解。先用带阻尼的 Verlet 积分施加重力、阵风与造型恢复力，再做 18 轮 XPBD 投影。隔一点的弦长控制弯曲，相邻点约束段长。乘子在一个子步内累积。一个 dispatch 最多合并八个待求解的子步。[求解着色器](../../shaders/hair_solve.comp) · [XPBD 原论文](https://matthias-research.github.io/pages/publications/XPBD.pdf)
3. **接触。** 每轮将自由点从头球和肩部胶囊内沿法线推出，接触余量为 8 mm。误差测在引导点上。线段和插值发丝仍可能局部穿过代理。当前实验的约束覆盖拉伸、近似弯曲和代理接触，发丝之间的碰撞、摩擦及扭转仍是研究空白。
4. **GPU 显示。** 粒子历史占 28 KiB。更新前从上一显示帧粒子生成旧姿态，更新后用当前切线相对静止切线的旋转和位置生成新姿态。每份矩阵占 56 KiB。四种发色和引导束视图共享同一份 GPU 结果，既有蒙皮 pass 直接读取矩阵。相邻引导束驱动 512 条渐细的三棱纤维，共 39,936 个三角形。可见性缓冲、PBR 材质与 TAA 沿用普通蒙皮表面的下游路径。每帧交回 CPU 的毛发诊断量只有一个最大伸长计数。[RDG 资源与 pass](../../src/experiments/hair/gpu.rs) · [姿态着色器](../../shaders/hair_palette.comp) · [蒙皮批次](../../src/render/geometry/skin.rs)
5. **连续性与存档。** 每个发型实例、每个视口独立保存 GPU 粒子历史。相机移动、分辨率变化和切换引导束显示均可继续求解。重复显示同一模拟时刻共用新旧姿态。重置或加载时废弃旧的运动对应。新存档记录固定时钟与输入变化；旧版 CPU 存档可用两组粒子位置作为起点。重新打开长存档时要从种子在 GPU 上重放积累的子步，首次显示时间随其增加。[渲染序列](../../src/render/post.rs) · [存档与数值回归](../../src/experiments/hair/tests.rs)

## 运行与复现

```powershell
cargo run --release --locked -- --experiment hair
```

| 控制台命令 | 观察内容 |
| --- | --- |
| `hair view guides` / `hair view strands` | 切换 64 条求解曲线与 512 条最终发丝。切换保留相同的模拟历史。 |
| `hair head off` / `hair head on` | 冻结当前头部角度或继续摆动。发丝仍受重力和风影响。 |
| `hair wind 0` / `hair wind 8` / `hair wind -8` | 关闭侧风，或沿两个相反方向施加阵风。范围为 −20 至 20 m/s²。 |
| `hair stiffness 0.2` / `hair stiffness 0.9` | 调节弯曲与造型保持，观察发梢偏移。 |
| `hair reset` / `hair status` | 保留参数并回到初始造型，或查看当前模拟时间及最近完成的主视口帧中的发段最大伸长百分比。 |

[同机位并排录像](../../captures/hair-guide-study.mp4) 在 0.8 秒开启正向风，2.8 秒反转，4.4 秒停风并开始转头。左右两次固定步长回放只切换显示方式。用下列命令复测数值与耗时。[测试与测量边界](../../src/experiments/hair/tests.rs)

```powershell
cargo test --release --locked --lib experiments::hair:: -- --include-ignored --nocapture --test-threads=1
```

## 数值和计时

同一套 release 试验分别测 CPU 调度和 GPU pass。CPU 使用 Ryzen 7 9700X、8 个 worker，预热 60 帧，采样 240 帧，每帧两个 120 Hz 子步。上一版 CPU 执行求解与姿态提交的 P50 / P95 是 0.231 / 0.317 ms。GPU 版这段 CPU 工作只更新固定时钟、控制输入和头部变换，本轮 P50 / P95 为 0.004 / 0.006 ms。

GPU 对照在 RTX 5070 上运行 125 个显示帧，同时记录全部 896 个引导点的当前与上一子步位置、前后显示帧的姿态、发根约束和接触穿入。测试包含 17 个子步的跳帧、同一时刻重复显示、窗口分辨率变化、冷重放、重置和旧版粒子存档恢复。两次模拟使用相同的动作和受力，逐点与旧 CPU 算法核对。CPU 参考算法仅编入测试程序。[完整测量方法与断言](../../src/experiments/hair/tests.rs)

| 指标 | 实测 |
| --- | --- |
| GPU 与 CPU 参考的最大粒子位置差 | 约 0.013 mm |
| 前后显示帧姿态变换还原引导点的最大差 | 小于 0.014 mm |
| GPU 发段最大伸长；自由引导点最大穿入 | 2.502%；约 0.00009 mm |
| CPU 参考单独运行 360 子步，默认转头和侧风 | 最大伸长 2.72%，发梢相对随头刚性造型的最大偏移 64.37 cm |
| 固定头部，侧风 −10 与 +10 m/s²，刚度 0.5 | CPU 参考抽样 16 束，最后 60 子步的发梢平均横移分别为 −23.43 cm 和 +26.64 cm |

GPU 对照选取预热后的 111 个常态双子步帧。求解 pass 的 P50 / P95 为 0.0014 / 0.0015 ms，上一帧与当前帧的姿态 pass 按帧相加后为 0.0028 / 0.0121 ms。求解 pass 的时间和两个姿态 pass 的合计时间使用 Vulkan 时间戳，测试的额外读回、网格蒙皮、着色及窗口呈现另外计时。
