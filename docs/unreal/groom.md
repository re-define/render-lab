# 毛发实验与 Unreal Groom

两边都用少量引导束计算动力学，再用引导束驱动更多显示发丝。最显著的差距出现在更换头模和处理扭转时。Render Lab 的程序化发型由头球和肩部胶囊定义；Epic Groom 将导入的造型、角色绑定、物理代理及求解设置作为可配置资产和组件。[本引擎发丝生成、约束和误差](../algorithms/hair-simulation.md) · [Epic Groom 物理设置](https://dev.epicgames.com/documentation/en-us/unreal-engine/enabling-physics-simulation-on-grooms-in-unreal-engine)

| 环节 | Render Lab 毛发实验 | Unreal Groom |
| --- | --- | --- |
| 求解 | 独立引导束在 GPU 上按固定时步做 Verlet 积分与 XPBD 投影。相邻点约束长度，隔点弦长近似弯曲。 | Niagara 在 GPU 上执行 XPBD，提供 Angular Spring、Cosserat Rod 等杆模型，并支持可配置的材料与碰撞参数。 |
| 碰撞 | 每轮从头球和肩部胶囊中推出自由点。测试测量引导点穿入和段长；插值纤维仍可能穿过代理。 | 可以指定 Physics Asset 参与 Groom 碰撞，配置碰撞和发束交互，使资产适配不同角色。 |
| 显示 | 前后两份显示姿态直接交给通用 GPU 蒙皮，最终以渐细三棱纤维进入可见性缓冲。 | Groom 提供造型、引导束和显示发丝的专门工作流，也能配置不同组的模拟设置。 |

上述 UE 能力依据 Epic 的[毛发物理概览](https://dev.epicgames.com/documentation/unreal-engine/hair-physics-in-unreal-engine---overview?application_version=5.8)与[Groom 模拟配置](https://dev.epicgames.com/documentation/en-us/unreal-engine/enabling-physics-simulation-on-grooms-in-unreal-engine)。

**建议先做绑定，再比较求解器。** 将发根局部框架、骨骼绑定和碰撞代理从试验头模提成一份可复用资产。以假发和运动角色各播同一套停风、反向风和快速转头输入，检查发根误差、插值后的可见穿入、GPU P95 及保存重载后的连续性。只有现有弯曲模型无法解释扭转误差时，再单独实验带方向框架的杆约束。这样可以区分错误来自资产绑定还是求解模型。[毛发对照方法](../algorithms/hair-simulation.md#数值和计时)
