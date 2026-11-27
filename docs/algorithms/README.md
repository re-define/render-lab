# 算法

| 研究问题 | 输入与输出 | 主要实验 |
| --- | --- | --- |
| [怎样构建和绘制稀疏植被](cluster-voxels.md) | 几何与表面属性 → 混合簇层级 → 每像素最近表面 | `clusters`、`forest-karst` |
| [怎样从可见性得到光照](visibility-lighting.md) | 表面 ID、深度、材质、光源和运动历史 → 直接光、间接光、重建颜色 | `restir`、`gi12-forest`、`mixed` |
| [怎样选出下一段动作](animation-physics.md) | 输入轨迹、脚的位置与速度、上一子步实际运动 → 基础姿态与期望根位移 | `motion`、`silver-contact` |
| [怎样给不同角色组合姿态](pose-graphs.md) | 基础动作、片段、骨骼遮罩和接触观测 → 角色的最终局部姿态 | `silver-contact`、`silver-contact-duo` |
| [怎样承重和对齐目标](physical-animation.md) | 根运动反馈、支撑面、动作姿态和末端目标 → 修正后的骨架 | `silver-contact`、`silver-contact-duo` |
| [发根运动怎样传到发梢](hair-simulation.md) | 固定时钟、头部姿态、风和约束 → 引导束、当前与上一显示帧的发丝姿态 | `hair` 毛发研究 |
| [怎样重建水面、碎块和压缩纹理](effects-textures.md) | 发射时间、几何单元或紧凑材质数据 → 可见表面或着色样本 | `water`、`destruction`、`texture`、`gaussian` |

算法页给出计算过程、误差来源和复现实验。更多阴影、卡通描边、独立光追和 IBL 的启动命令见[实验目录](../experiment-catalog.md)。要追踪结果如何跨线程、帧槽或队列传递，可转到[架构图](../architecture/README.md)。
