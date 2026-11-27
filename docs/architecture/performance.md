# 性能指标与复现

这里的参考数据来自 Windows、Ryzen 7 9700X、RTX 5070 的 release 构建。500 人 CPU 数据对应 `7b84d04`，300 人绘制与森林队列对照对应 `4b6ed7c`。每组保留各自的输入、样本数和统计边界。低分辨率 GPU 回归另作正确性验证，不参与本页的帧率计算。

## 先确定计时区间

| 指标 | 计量方法 | 适合回答的问题 |
| --- | --- | --- |
| 模拟、提取 P95 | 对每个样本分别计一次系统推进和一次构造只读快照。对各自样本排序。 | 修改物理、动画或 ECS 后，主线程等待多久能得到下一份快照？ |
| 窗口完成间隔、吞吐 | 用相邻已退役帧的 CPU 时刻相减。整个采样窗口内以完成帧数除以时间得到吞吐。 | 窗口是否稳定交付，包括队列等待和帧槽背压？ |
| 整帧 GPU 跨度、pass 时长 | 整帧使用可对齐的 GPU 时间戳。每个 pass 在所属队列计时，查看区间并集与等待来源。 | GPU 花了多久，不同物理队列上的工作实际重叠多少？ |
| worker 的 CPU-ms | 在采样窗口内取得各线程累计占用核心的时间，再除以完成帧数。 | 模拟、构图、录制的工作量怎样分布？多核并行会使总 CPU-ms 大于一帧的墙钟时间。 |

分位数应来自同一组逐帧样本。整帧 P95 取自每一帧独立测得的 GPU 跨度。窗口帧率取自整段时间内的完成事件。交互窗口输入 `timeline target/frame.json` 导出最近一帧的队列时间线。启动时加入 `--benchmark 96` 会预热 60 个完成帧后输出 96 个样本的 JSON，其中 `frame_times` 给出逐帧事件，`stats` 给出汇总，`passes` 给出最后一帧的父依赖、队列、屏障与区间。[窗口基准实现](../../src/ui/benchmark.rs) · [时间线导出](../../src/rhi/timeline.rs)

## 银狼人群

**固定 CPU 工作量。** 500 名银狼、177,042 个实体、3,000 个蒙皮实例，16 个 worker。每个样本用 0.1 秒模拟时间触发六个 60 Hz 子步，再提取一次。预热 20 个样本，保留 96 个。计时区外把首末采样帧与独立全量提取逐实例核对根变换和调色板。[基准实现](../../src/experiments/motion/silver/contact/crowd_profile.rs)

| 同机工作量 | 六子步模拟 P95 | 提取 P95 |
| --- | ---: | ---: |
| 世界运动与时钟重构前，两次交替对照 | 26.76–37.59 ms | 7.68–9.54 ms |
| 世界运动与时钟重构后，两次交替对照 | 27.02–32.96 ms | 8.34–8.80 ms |
| `7b84d04` 最终复验，单次 | 28.95 ms | 8.22 ms |

最终复验的模拟与提取 P50 分别为 25.12 ms 和 7.51 ms，每样本读取 500 个姿态块、10,500 个脏关节局部变换，仍需检查 159,500 个调色板关节。交替对照的 P95 区间重叠，无法据此认定运动与时钟改造提高了 CPU 速度。

**固定窗口与绘制路径。** 另一个基准使用 300 人、1920×1080、MAILBOX 和两个在途槽。场景按实际经过时间推进，预热 20 个完成帧，采样 96 帧。两组均运行于 `4b6ed7c`，只切换普通绘制路径。[绘制工作组织](../../src/render/draw_list.rs) · [冻结姿态的像素回归](../../src/experiments/motion/silver/contact/crowd_draw_test.rs)

| 指标 | 逐部件绘制 | GPU 生成绘制列表 |
| --- | ---: | ---: |
| 普通 draw 数／动态渲染作用域数 | 1,824 | 10 |
| 整帧逻辑 pass 数 | 1,843 | 49 |
| CPU 构图与编译 P95 | 6.31 ms | 5.04 ms |
| 整帧 GPU P50 / P95 | 8.60 / 9.06 ms | 3.62 / 3.82 ms |
| 完成间隔 P95 | 27.32 ms | 16.65 ms |
| 平均完成帧率 | 63.66 帧/秒 | 100.36 帧/秒 |

从仓库根执行以下 PowerShell 命令。设置 `RENDER_LAB_REFERENCE_DRAWS` 切换逐部件对照，取消变量后恢复 GPU 列表。窗口场景会统计至少覆盖一个像素的角色数量，读回操作位于采样区外。

```powershell
$env:RENDER_LAB_NPCS = '500'
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture
$env:RENDER_LAB_NPCS = '300'
cargo test --release --locked --lib silver_crowd_window_profile -- --ignored --nocapture
$env:RENDER_LAB_REFERENCE_DRAWS = '1'
cargo test --release --locked --lib silver_crowd_window_profile -- --ignored --nocapture
Remove-Item Env:RENDER_LAB_REFERENCE_DRAWS
Remove-Item Env:RENDER_LAB_NPCS
```

## 角色反馈和共享碰撞的开销

对照使用 `8ad4259` 和本次实现，各自独立编译。两端均使用上面的 500 人工作量和无限额姿态提取，默认局部物理动画关闭。顺序为旧、新、新、旧。每一行是一次独立运行，单位为毫秒。分别按排序后的 96 个样本取最接近 `q × (N − 1)` 的样本。每个采样窗口计六个子步和一次场景提取，不包含窗口绘制。

| 版本及局部物理动画 | 六子步模拟 P50 / P95 | 随后提取 P50 / P95 |
| --- | ---: | ---: |
| 基线，第 1 次 | 20.98 / 24.51 | 6.93 / 8.29 |
| 当前，关闭，第 1 次 | 22.84 / 25.32 | 7.15 / 7.84 |
| 当前，关闭，第 2 次 | 22.89 / 29.34 | 7.06 / 9.39 |
| 基线，第 2 次 | 20.82 / 22.15 | 6.86 / 7.43 |
| 当前，500 人全部开启，第 1 次 | 23.99 / 28.52 | 7.01 / 8.52 |
| 当前，500 人全部开启，第 2 次 | 24.09 / 30.08 | 7.19 / 9.37 |

新版本在同一范围内额外求解陌生碰撞体、移动父节点、胶囊侧壁和顶部接触，并把实际位移、支撑点速度、阻挡法线写回动作查询。默认三条局部物理骨链均关闭；打开后建立每人的三条关节约束链。两种配置都逐实例与独立冷提取比对。单轮分段诊断只用于判断工作发生在哪里，记录提交与撤销需要额外计时。

首个采样 tick 提交 25,206 项写入，其中组件 18,600 项，姿态矩阵载荷 4,262,400 字节。为支持失败后的子步回滚准备了 19,206 条记录，其中矩阵 230,400 字节。诊断模式下六个子步的 worker 求值、提交、撤销准备 P50 分别为 9.81、8.18、3.63 ms，计时互相排除；没有实际回滚。默认路径的两次模拟 P50 比同机基线高约 2 ms。两轮 P95 波动更大，可看出完整人群这次仍有额外 CPU 成本。

另测 3,000 个盒碰撞体，其中 300 个每步移动，500 次胶囊扫掠，每次平均进入窄相位 1.00 个候选。每项的 P50 / P95 依次为共享快照固定步骤 0.437 / 0.556 ms、查询与支撑面组装 0.139 / 0.277 ms、轴对齐盒精确求交 0.016 / 0.019 ms。该组使用两个 worker，测量 96 次。

```powershell
$env:RENDER_LAB_NPCS = '500'
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture
$env:RENDER_LAB_PHYSICAL_LIMBS = '1'
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture
Remove-Item Env:RENDER_LAB_PHYSICAL_LIMBS, Env:RENDER_LAB_NPCS
cargo test --release --locked --lib profile_dynamic_collision_snapshot -- --ignored --nocapture
```

[子步交换、开关和视频](character-motion.md) · [障碍场的动作精度对照](../algorithms/animation-physics.md) · [人群基准](../../src/experiments/motion/silver/contact/crowd_profile.rs)

## 岩柱森林的物理队列

相同的 420 帧相机路径，原生 1080p、TAA、ReSTIR、硬件直接光阴影和多跳间接光。剔除开头 48 帧，统计 372 帧。两个在途槽，队列数在设备创建时确定。[相机轨迹和逐帧 CSV](../../examples/forest_shadows/zoom.rs)

| 配置 | GPU P50 / P95 | 完成间隔 P50 / P95 | 提交等待中位数，含 WSI | 资源屏障中位数 |
| --- | ---: | ---: | ---: | ---: |
| 自动选择的单队列 | 20.04 / 23.65 ms | 20.14 / 23.78 ms | 1 | 240 |
| 显式三队列 | 22.42 / 25.73 ms | 21.23 / 25.36 ms | 9 | 231 |

该路径上的主视图网格和 BLAS 构建区间重叠 P95 均为 0 ms。瞬态设备本地资源、加速结构和构建 scratch 峰值分别为 1,805.11、289.95、17.99 MiB，两组相同。三队列在这台机器上减少了部分屏障，增加了等待，最终 GPU P95 增长 2.08 ms。重新运行会在两个目录分别生成 `zoom.csv`，可按相同相机阶段配对。

```powershell
cargo run --release --locked --example forest_shadows -- target/forest-auto zoom 1 --size 1920x1080
$env:RENDER_LAB_GPU_QUEUES = '3'
cargo run --release --locked --example forest_shadows -- target/forest-q3 zoom 1 --size 1920x1080
Remove-Item Env:RENDER_LAB_GPU_QUEUES
```

## 森林稀疏提取

这项 CPU 基准使用 `vegetation-scale`，包含 2,665 棵树、12,580 丛草、17,939 个实体。每种输入轮换三次，每轮预热后计 200 个样本。每次都读取提交后的快照并核对树的变换和共享树皮，保留两份旧快照来覆盖跨帧读者。[提取基准与 CSV 字段](../../examples/forest_extraction.rs)

| 操作 | 访问实体 | 更新实例 | 提取 P95 |
| --- | ---: | ---: | ---: |
| 只移动相机 | 0 | 0 | 0.0003 ms |
| 移动一棵树，含树干和树冠 | 2 | 2 | 0.0084 ms |
| 修改 733 个实例共享的树皮材质 | 1 | 0 | 0.0011 ms |

```powershell
cargo run --release --locked --example forest_extraction > target/forest-extraction.csv
cargo run --release --locked --example forest_scene_updates -- target/forest-uploads.csv
```

第二条命令另外检查 GPU 表更新和动态上传字节，并与全量打包的图像进行对照。实际上传费用与实例提取次数分别记录。[上传验证](../../examples/forest_scene_updates.rs)

## 稳定人群图的编译成本

同一份 release 二进制，Ryzen 7 9700X、RTX 5070、NVIDIA 621.14，300 名银狼，1920×1080 的窗口交换链。每组预热 20 帧，保留 96 个完成帧。只切换 `RENDER_LAB_RDG_PLAN_CACHE=0|1`。构图计时包含蒙皮批次和帧输出的最终装配。各列是同一段采样的 P50 / P95，单位为毫秒。

| 每视图模板 | 当帧构图 | 依赖编译 | 逐帧构图与编译合计 | GPU 跨度 | 完成间隔 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 关 | 4.61 / 5.48 | 1.34 / 1.70 | 6.07 / 6.98 | 3.58 / 3.79 | 17.43 / 29.69 |
| 开 | 4.57 / 5.32 | 0.22 / 0.27 | 4.79 / 5.55 | 3.64 / 3.87 | 15.03 / 25.90 |

本次平均吞吐从 71.52 提升到 79.66 个完成帧/秒。固定姿态的逐像素验证覆盖表面 ID、深度、运动和历史对应；其读回在独立用例中执行。[窗口采样](../../src/experiments/motion/silver/contact/crowd_profile.rs) · [像素验证](../../src/experiments/motion/silver/contact/crowd_draw_test.rs)

## 森林 RDG 与物理队列对照

Ryzen 7 9700X、RTX 5070、NVIDIA 621.14，1920×1080、MAILBOX、硬件光追和森林默认后处理。四组各用一圈相同的 420 帧相机路径，舍弃起始 48 帧，保留 372 帧。三个物理队列属于同一个通用队列族。表中各项分位数由逐帧记录独立计算。

| 设置 | 编译命中 | CPU 依赖编译 P50 / P95（ms） | 整帧 GPU P50 / P95（ms） | 时间线等待 P50 | 主光栅与 BLAS 重叠 P95（ms） |
| --- | ---: | ---: | ---: | ---: | ---: |
| 自动映射到单队列、关闭模板 | 0 / 372 | 0.286 / 0.488 | 20.83 / 24.14 | 1 | 0.00 |
| 自动映射到单队列、启用模板 | 282 / 372 | 0.080 / 0.503 | 20.93 / 24.17 | 1 | 0.00 |
| 三队列、优先同队列 | 282 / 372 | 0.077 / 0.500 | 21.80 / 25.13 | 9 | 0.00 |
| 三队列、优先最长下游依赖链 | 282 / 372 | 0.078 / 0.551 | 21.56 / 24.51 | 9 | 0.00 |

在固定远景的 48 帧内，开启模板后 CPU 依赖编译 P95 从 0.452 ms 降到 0.094 ms。推近与返回阶段拓扑频繁变化，两段的命中率分别为 40% 和 35%。全路径的编译均值从 0.319 ms 降到 0.158 ms，P95 略升。自动策略保留这台设备上森林的单队列映射。单队列仍有来自其他提交或资源就绪状态的时间线等待。可在输出目录的 `timeline-120.json` 与 `timeline-400.json` 检查实际队列和完成链。[队列策略](../../src/rhi/queues.rs) · [回放实现](../../examples/forest_shadows/zoom.rs)

```powershell
cargo run --release --locked --example forest_shadows -- target/forest-auto zoom 1 --size 1920x1080
$env:RENDER_LAB_GPU_QUEUES = '3'
cargo run --release --locked --example forest_shadows -- target/forest-three zoom 1 --size 1920x1080
$env:RENDER_LAB_RDG_ORDER = 'critical'
cargo run --release --locked --example forest_shadows -- target/forest-critical zoom 1 --size 1920x1080
Remove-Item Env:RENDER_LAB_RDG_ORDER, Env:RENDER_LAB_GPU_QUEUES
$env:RENDER_LAB_RDG_PLAN_CACHE = '0'
cargo run --release --locked --example forest_shadows -- target/forest-uncached zoom 1 --size 1920x1080
Remove-Item Env:RENDER_LAB_RDG_PLAN_CACHE
```

## 银狼姿态图的 CPU 开销

对照版本为 `bf95a88`。在 Ryzen 7 9700X 上用 Release 模式和最多 16 个 worker 运行同一人群工作负载。每个配置独立运行五次，按不同顺序交错；每次预热 20 帧、测 96 帧。每个采样帧模拟 0.1 秒，共六个固定子步，再提取一次 1920×1080 视图。表内单位为毫秒，前三个数据列均取五次 P95 的中位数。提交计时属于模拟的一部分。此基准只执行 CPU 模拟与快照提取。[原始各轮分位数](measurements/silver-pose-graph.csv) · [基准程序](../../src/experiments/motion/silver/contact/crowd_profile.rs)

| 角色数 | 配置 | 模拟 P95 | 提交 P95 | 提取 P95 | 五轮模拟 P95 范围 |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 旧版固定顺序 | 0.43 | 0.03 | 0.03 | 0.41–0.45 |
| 1 | 默认图 | 0.43 | 0.03 | 0.03 | 0.41–0.44 |
| 1 | 九通道上半身图 | 0.43 | 0.03 | 0.03 | 0.42–0.47 |
| 500 | 旧版固定顺序 | 25.51 | 9.13 | 7.80 | 24.39–29.90 |
| 500 | 默认图 | 25.85 | 8.91 | 7.73 | 24.45–30.74 |
| 500 | 九通道上半身图 | 28.36 | 9.97 | 7.63 | 24.31–29.72 |

500 人的三组模拟 P95 区间有重叠。这组数据可以用来估算启用分层时的总预算，尚不足以归因出稳定的整段加速。另一项同机位的像素对照和双人姿态回归用于检查动作正确性。[姿态图与录像](../algorithms/pose-graphs.md)

```powershell
$env:RENDER_LAB_SIM_PROFILE = '1'
$env:RENDER_LAB_NPCS = '500'
$env:RENDER_LAB_POSE_GRAPH = 'upper'
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture --test-threads=1
Remove-Item Env:RENDER_LAB_POSE_GRAPH, Env:RENDER_LAB_NPCS, Env:RENDER_LAB_SIM_PROFILE
```

## 统一世界时间与姿态求值

这次以 `a471298` 的姿态图为基线。两版在同一台 Ryzen 7 9700X、Release、同一组 16 个 worker 上运行，默认图和上半身图各自新建场景。每个配置与基线交错运行五轮，每轮预热 20 帧、采样 96 帧。每个样本推进六个 60 Hz 角色子步，再执行一次 CPU 场景提取。表中为五轮 P95 的中位数，单位为毫秒，箭头左边是基线，右边是当前实现。worker 为所有模拟阶段工作批次的计时，提交属于整段模拟。[逐轮数据](measurements/world-time-pose-eval.csv) · [人群基准](../../src/experiments/motion/silver/contact/crowd_profile.rs)

| 人数 | 配置 | 模拟 P95 | worker 求值 P95 | ECS 提交 P95 | 提取 P95 |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 默认图 | 0.44 → 0.43 | 0.36 → 0.36 | 0.03 → 0.03 | 0.03 → 0.03 |
| 1 | 上半身图 | 0.44 → 0.44 | 0.37 → 0.37 | 0.03 → 0.03 | 0.03 → 0.03 |
| 500 | 默认图 | 24.15 → 23.19 | 9.95 → 9.20 | 8.37 → 8.52 | 7.46 → 7.40 |
| 500 | 上半身图 | 24.38 → 23.96 | 10.16 → 9.52 | 8.45 → 8.53 | 7.45 → 7.54 |

在这组数据中，500 人默认图和上半身图的工作线程求值 P95 分别减少 0.75 和 0.64 ms。上半身图的整段模拟 P95 减少 0.42 ms，提交与提取各自略有波动。单个角色的结果在 0.01 ms 的报告精度内接近基线。

为了区分图求值和 ECS、物理的成本，另在一个线程上连续求 500 份姿态。每个样本恢复同一份起始局部与世界矩阵。标量参考在每层之后计算全部 31 个绑定关节的世界矩阵，实际执行器每个连续混合组更新 14 个受影响关节。双层测试叠加两个不同片段，测量期间两层均处于满权重窗口。同相用相同小数帧，错相则为角色错开片段相位。每轮交替测量参考与实际实现，预热 20 次、采样 96 次。单位为毫秒。[等价性检查与内核基准](../../src/experiments/motion/silver/contact/rig/evaluation_tests.rs)

| 混合层数 | 播放相位 | 参考 P50 | 新版 P50 | 参考 P95 | 新版 P95 |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 同相 | 0.350 | 0.239 | 0.370 | 0.254 |
| 1 | 错相 | 0.390 | 0.382 | 0.401 | 0.392 |
| 2 | 同相 | 0.698 | 0.441 | 0.723 | 0.463 |
| 2 | 错相 | 0.699 | 0.623 | 0.718 | 0.651 |

两种内置图的同机位撑栏静帧（640×480，关闭后处理，16 帧预热）分别和基线逐像素相同。内核只测局部混合与矩阵传播，整段模拟还包含动作查询、接触、IK 和 ECS 提交。

```powershell
cargo test --release --locked --lib silver_masked_evaluation_profile -- --ignored --nocapture --test-threads=1
$env:RENDER_LAB_SIM_PROFILE = '1'
$env:RENDER_LAB_NPCS = '500'
$env:RENDER_LAB_POSE_GRAPH = 'upper'
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture --test-threads=1
Remove-Item Env:RENDER_LAB_POSE_GRAPH, Env:RENDER_LAB_NPCS, Env:RENDER_LAB_SIM_PROFILE
```

## 仓库附带的数据

[crowd-frame-metrics.csv](measurements/crowd-frame-metrics.csv) 保留更早一次银狼蒙皮批处理的采样汇总，列包括人数、测试版本、工作负载、样本数、P50、P95 与均值。`current` 是那次测量的对照版本标签。逐线程均值保存在 [npc-threads.csv](measurements/npc-threads.csv)，单位为每完成帧的 CPU 毫秒。复测时请另存提交号、驱动、开关、分辨率及各帧输出。

## 资源管理与流式加载实验

同机 Release，旧版为 `f8d8a214add`。500 名银狼、177,042 个实体、3,000 个蒙皮实例，20 次预热，96 个样本，每样本推进六个子步。开启提交计数，两次抽取全场景与增量结果逐矩阵核对。每个样本读取 500 个姿态块、10,500 个变化局部矩阵，检查 159,000 个调色板关节。独立角色在引擎已有的工作池中更新调色板，主线程合并实例结果。[人群用例](../../src/experiments/motion/silver/contact/crowd_profile.rs)

| 指标 | 旧版 | 本次 |
| --- | ---: | ---: |
| 模拟 P50 / P95 | 21.89 / 23.74 ms | 19.81 / 21.51 ms |
| 提取 P50 / P95 | 6.98 / 7.43 ms | 5.81 / 6.26 ms |
| 提交 P50 / P95 | 7.93 / 8.61 ms | 5.92 / 6.44 ms |

森林 CPU 提取独立使用 2,665 棵树、12,580 丛草，每种操作 600 个样本。相机移动访问 0 个实体、更新 0 个实例，P95 为 0.0002 ms。单树位置变化访问 2 个实体、更新 2 个实例，P95 为 0.0044 ms。共享树皮变化访问 1 个材质实体、更新 0 个实例，P95 为 0.0009 ms。[提取基准](../../examples/forest_extraction.rs)

森林 GPU 对照使用 RTX 5070、1920×1080、MAILBOX、同一条 `forest_shadows zoom` 路径，各录 420 帧。GPU 分位数排除开头 48 个预热帧，设备高水位包含预热。容量兼容的 RDG 瞬态别名使 Vulkan 设备本地分配高水位从 2,458.37 MiB 降到 2,410.91 MiB。GPU P50 / P95 从 20.06 / 23.58 ms 变为 21.18 / 24.92 ms。这组测量有显存收益，GPU 耗时增加，旧森林未接入新的流式包。[森林轨迹基准](../../examples/forest_shadows/zoom.rs)

上一版（`7db5531`）的 `streaming` 硬件回归使用 320×180、每帧最多上传 1 页，依次绘制常驻粗级、逐级补齐、保留旧图并转远、最后释放整个资产。精细级与粗级有 24,113 个不同像素。共上传 9 页，已绑定细页峰值 589,824 B，专用 Vulkan 块峰值 2,097,152 B。去掉旧帧的最后引用后两项均为 0 B。退场前构建的旧帧和退场后的粗级帧同时进入两个在途槽，旧帧仍逐像素重放精细结果。DirectStorage 与普通后台读盘各跑一次，两者精细图像的 SHA-256 相同。640×360 的 CLI 捕获在第 35 帧仍使用粗级，第 500 帧贴图、几何和关节均已达到第 2 级。预热帧数不保证 I/O 完成。启动费时、驱动抖动、SSD 到显存的实际带宽以及真实森林的缺页率需要单独测量。[硬件回归](../../src/experiments/streaming_lab.rs)


### 可见页反馈和设备级预算

相同 RTX 5070、320×180、同一织纹用例，每个主视图最多上传一页。将 4 关节和 16 关节的两个精细载荷内联后，严格 DirectStorage 路径从 9 次成功上传降为 7 次，细页从 576 KiB 降为 448 KiB。两个版本的精细 BGRA 像素 SHA-256 均为 `ce3c55569a186cebfb90e74193930e9740b0bff305091249bcc6d750e4a267c6`。承诺物理块峰值均为 2 MiB，退场后细页和块均回到 0。另一项硬件回归在一个原生批次复制三页，帧尚未退役时检查复制时间线，三页均已发布。[像素回归](../../src/experiments/streaming_lab.rs) · [时间线回归](../../src/rhi/streaming.rs)

设备压力测试让两个管理器各保留一个 64 KiB 细页，保留旧帧地址表后将设备流式预算压到 64 KiB。RHI 请求驱逐冷页，旧帧仍引用它时，绑定总量为 128 KiB，其中 64 KiB 等待退役。释放旧帧后，热页仍可绘制，总绑定降至 64 KiB，待退役为零。主动收紧预算时实际用量会在旧帧完成前暂时高于新上限。[设备压力回归](../../src/rhi/streaming.rs)

世界 CPU 测试使用三个各含 768 个独立实体的区域，每个区域 24 批。计时覆盖主线程 `Runtime::update`，模拟、GPU 和每次 tick 间的 1 ms 等待另计。单次 Release 回放的结果如下。复现命令为 `cargo test --release --locked --lib cold_revisit_and_teleport_world_cpu_cost -- --ignored --nocapture`。[分区基准](../../src/scene/partition/tests.rs)

| 操作 | 收集的更新次数 | 主线程 P95 | 主线程 P99 |
| --- | ---: | ---: | ---: |
| 冷入 | 25 | 0.1355 ms | 0.1393 ms |
| 向前移动 | 147 | 0.1656 ms | 0.2173 ms |
| 原路折返 | 153 | 0.3470 ms | 0.4232 ms |
| 瞬移 | 158 | 0.1664 ms | 0.1935 ms |


### 两个相邻分区共享一片橡树林

RTX 5070、960×540，离屏回放包含冷入、折返、两区共存、瞬移与卸载。每轮 260 帧，引擎资产页池重新创建；操作系统文件缓存和 Vulkan 驱动缓存可能已热。保留窗口按 0、75、75、0 帧交叉排列，每种策略各两次。表中 P99 覆盖该轮全部 260 帧。`tick_to_completion` 包含 CPU、线程排队和离屏读回，和 GPU 时间戳跨度分列。

| 回放顺序 | 冷页保留帧数 | 重读有效载荷（MiB）| 首次可用 P95 | 全路径 GPU P99 | Tick 至完成 P99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 0 | 7.87 | 221.921 ms | 3.939 ms | 22.712 ms |
| 2 | 75 | 1.79 | 199.211 ms | 3.920 ms | 23.010 ms |
| 3 | 75 | 1.79 | 218.729 ms | 4.659 ms | 27.467 ms |
| 4 | 0 | 7.87 | 224.458 ms | 4.379 ms | 24.311 ms |

两轮立即回收均重读 8,250,672 B，保留 75 帧均重读 1,872,384 B，减少 77.3%。两种策略的细页和页池块峰值均为 8 MiB，完整退出两区后，绑定细页、页池块、常驻根、待退役页和上传暂存都归零。短时保留稳定减少了重复读取，GPU 和整帧 P99 在不同轮次有波动。按策略合并各 520 帧，GPU P99 从 4.330 ms 变为 4.191 ms，tick 至离屏完成 P99 从 24.231 ms 升至 26.421 ms。此次回放使用 24 棵共享同一原型的树，测量范围不含完整岩柱森林的风、阴影和 GI。[逐帧原始数据](measurements/forest-stream.csv) · [回放程序](../../examples/forest_stream.rs) · [森林流式算法](../algorithms/forest-streaming.md)
