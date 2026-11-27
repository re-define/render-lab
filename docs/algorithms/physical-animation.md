# 支撑冲量、物理骨链与末端约束

角色的一次子步会同时产生根运动和显示姿态。下面说明承重、骨链和 IK 怎样分别改变这两类输出。[同一子步的交换协议](../architecture/character-motion.md)

## 支撑、握持与局部物理动画

撑栏时先在动作事件窗口内用动画姿态检查手是否能到达栏杆，得到下一步可用的承重比例。求解器再检查支撑实体、碰撞层和角色是否离地，计算该步向上的冲量。手部 IK 单独负责把手掌贴上栏杆。实验可固定机械承重开关，只切换手部 IK，逐步比较腕部姿态、根高度和手部冲量。[撑栏接触](../../src/experiments/motion/silver/contact/traversal.rs) · [角色根求解](../../src/experiments/motion/silver/contact/dynamics/root.rs) · [成对回放测试](../../src/experiments/motion/silver/contact/exchange_tests.rs)

`contact limbs on` 打开局部物理动画，`contact limbs 0.5` 混合一半，`contact limbs off` 返回默认姿态。右臂和两条腿各有一条两段骨链。肩或大腿根跟随动作，其余两个节点受重力、阻尼、骨长约束和带柔度的姿态目标驱动，并与查询到的可行走表面接触。每个内部时间步最多 1/120 秒，用八轮迭代求解。右臂、右腿和左腿分别保存驱动柔度（m/N）、速度阻尼（每秒）和可选屈曲范围。默认柔度 0.008、阻尼 5、角度不限。旧存档采用这组默认值。关闭物理混合会清掉模拟位置和速度，自定义参数仍随角色保存。[驱动参数](../../src/physics/articulation/drive.rs) · [骨链积分](../../src/physics/articulation.rs)

屈曲角是近端骨骼方向和远端骨骼方向的夹角，完全伸直为 0°。每轮在骨长修正之后检查夹角，超过范围时把远端方向投回允许的锥面，保留当时的远端骨长。共线时先参考上一子步的弯曲方向，再参考动画目标。在同一轮中，接触面投影随后执行，接触与限位冲突时可能产生残差。该模型约束弯曲幅度；铰链轴、扭转和全身布娃娃仍需更完整的关节模型。[角度投影](../../src/physics/articulation/drive.rs)

配置随角色保存。默认姿态图先混合物理结果，再执行手脚 IK，后者可改变最终关节角。控制台输入 `contact limbs` 显示的是各条物理骨链的实际角度。要观察限位对显示骨架的直接影响，可使用以下设置。在 60 Hz、90 个子步的银狼回归中，手脚 IK 关闭，没有限制时发布的右肘角为 1.97°–11.48°，设为 80°–90° 后为 80.00°–90.00°。两组根变换逐步相同。[骨架回归](../../src/experiments/motion/silver/contact/physical_pose/tests.rs) · [姿态图](pose-graphs.md)


同一机位分别录制 `contact limbs off` 与 `contact limbs on` 的翻栏，保持手脚 IK 和机械撑栏设置一致。画面左侧为默认动画，右侧为三条骨链全部混合。两组各为 30 fps、63 帧。[翻栏并排录像](../../captures/silver-physical-limbs.mp4)

## 如何检查效果

```powershell
cargo run --release --locked -- --experiment silver-contact
```

| 窗口控制台命令 | 观察内容 |
| --- | --- |
| `contact action vault`，`contact hand off` | 保留机械撑栏，观察手腕脱离视觉目标时的根轨迹。 |
| `contact hand on`，`contact grip off`，再运行 `contact action vault` | 保留手掌对齐，观察缺少向上支撑冲量的表现。 |
| `contact limbs on`，`contact feet off`，`contact hand off` | 关闭末端覆盖，观察重力和姿态驱动产生的骨骼运动。 |
| `contact limbs arm limit 80 90`，`contact limbs` | 将右肘物理屈曲限定在 80°–90°，查看求解角度。腿链名为 `right-leg` 和 `left-leg`。 |
| `contact limbs arm drive 0.02 8`，`contact limbs arm limit off` | 修改右臂柔度和阻尼，再移除角限位。 |
| `motionroot controller` 或 `motionroot clip` | 切换所选角色的平面根运动来源。 |
| `contact mode free` 或 `contact mode contact` | 切换独立移动与物理接触，沿用已选角色与运动状态。 |
| `schedule profile on`，随后 `schedule profile` | 查看调度、提交、撤销准备与写入计数。姿态字节数只统计矩阵载荷。 |

共享盒碰撞索引和扫掠在物理层实现。角色使用直立胶囊，支撑/实体盒/交互锚点可以独立配置。扫掠连续处理相对平移；快速旋转的侧壁按子步采样朝向。任意三角网格碰撞与连续旋转体求交尚待实现。旋转平台的支撑点单独使用角速度计算。与角色耦合的弹簧板目前要求共用父坐标系。[盒碰撞体和索引](../../src/physics/collision.rs) · [胶囊扫掠](../../src/physics/sweep.rs) · [每子步的碰撞快照](../../src/experiments/motion/silver/contact/collision.rs)

运动墙即使与远处碰撞体的增删发生在同一子步，仍需要保留前一帧的位置才能捕获横穿角色的碰撞。热重绑按实体代次、父链和形状匹配样本。换父、复用 ID 或编辑形状会让对应历史失效。运动停止后，索引收窄上一子步的扫掠包围盒，再复用静态快照。[历史重映射与热重绑测试](../../src/experiments/motion/silver/contact/collision/history.rs)
