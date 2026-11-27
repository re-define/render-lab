# 实验目录

从仓库根目录运行。下列命令依次列出本次构建的全部具名预设、打开岩柱森林、从存档恢复。省略预设和存档时打开默认的双材质演示。窗口中用 WASD 移动相机，按住鼠标右键转向，按 F 重新取景，按 `/` 打开控制台。银狼的移动输入使用 IJKL。[启动参数](../src/cli.rs) · [预设与文件加载](../src/presets.rs) · [窗口输入](../src/ui.rs)

```powershell
cargo run --release --locked -- --list-experiments
cargo run --release --locked -- --experiment forest-karst
cargo run --release --locked -- --scene target/saved.json
```

## 几何、运动与场景组合

| 预设 | 资产和要观察的现象 | 建议的同机位操作 |
| --- | --- | --- |
| `clusters` | 多个 Dragon 实例和普通网格同屏。GPU 按屏幕误差遍历几何层级。 | `lodview on` 显示所选层级，`lod 0.25` 修改误差阈值。 |
| `vegetation` | 程序化树和草。枝叶可在同一层级中选择三角簇或体素簇。 | `vegetation view paths` 后在 `vegetation auto` 和 `vegetation triangles` 间切换。 |
| `vegetation-scale` | 放大到林地规模，共享树干、树冠、草丛网格和材质。 | `vegetation wind 0` 固定风，再调 `vegetation pixels 1` 观察选簇变化。 |
| `river-forest` | 稠密河谷，生成器回归检查至少两万棵树、二十万丛草，并保留水道和河岸。 | `vegetation camera river` 看河道，`vegetation camera grass` 检查地表。 |
| `forest-karst` | 岩柱、河谷植物、太阳、ReSTIR 和缓存间接光。预设要求硬件光追，CLI 默认选择 TAA。 | `vegetation camera karst` 固定机位，分别输入 `restir gi direct`、`restir gi multi`。 |
| `movement` | 三个共享网格的物体分别静止、水平移动、受重力落下。 | `object` 列出 ID，`object 3` 查看策略。初始状态输入 `object 6 impulse 0 2 0 at 3.5 2 0` 可以观察右侧物体受冲量并旋转，同时听到一次撞击音。用 `audio cues off` 关闭场景音效。`tick movement 30` 调整普通运动频率。 |
| `motion` | 银狼骨架和烘焙的动作库，观察输入轨迹、姿态候选和转向。 | 用 IJKL 改方向，再用 `motionsearch kd`、`motionsearch scan` 对照查询。 |
| `hair` | 程序化假发、头模与肩部胶囊；侧风、头部转动和造型刚度可以独立调节。 | `hair status` 查看最大伸长，`hair wind -8` 反转侧风。[毛发模拟研究与操作](algorithms/hair-simulation.md) |
| `silver-contact` | 银狼通过平台、栏杆和低矮障碍，用接触反馈修正脚掌和末端触点。 | `contact graph upper` 切换上半身混合，`contact graph default` 恢复；`contact status` 查看状态，`contact limbs on` 打开局部物理动画，`contact limbs arm limit 80 90` 设置肘部屈曲范围，`contact grip off` 关闭机械撑栏。[子步与实验命令](architecture/character-motion.md) |
| `silver-contact-duo` | 双角色障碍路线。两者共享可见网格，第二名角色采用不同的腕部关节链与指尖绑定。 | `control` 查询角色 ID，`control ID` 选择角色，`drive 0.25 0.8` 输入连续方向，`motionroot clip` 采用片段根运动，`contact mode free` 切到独立移动。 |
| `water` | 程序化台阶上，计算着色器沿流线推进不同年龄的水团，再从相邻采样生成水面。 | `effect restart` 重新开始，`effect time 2` 指定观察时刻。 |
| `destruction` | 程序化碎块和消融表面。比较相同几何在完整、破碎和消融阶段的轮廓。 | 依次输入 `effect restart`、`effect time 2.2`、`effect time 4.7`。 |
| `confluence` | 同一 ECS 世界装配森林、流水、银狼、IBL 与多光源直接光。 | `feature water off`、`feature actor off`、`feature restir off` 分别关闭一层，相同命令改成 `on` 可恢复。 |

几何树与材质、运动与碰撞各自有独立入口。林地样本由种子生成，截图对照时可固定风时间。银狼运动时钟与物理状态可通过 `save target/saved.json` 保存，再用 `load target/saved.json` 接续。[植物生成](../src/experiments/vegetation.rs) · [稠密河谷规模回归](../src/experiments/vegetation/river_forest.rs) · [特效实现](../src/experiments/effects.rs) · [角色求解](algorithms/animation-physics.md)

## 材质、压缩、直接光与间接光

| 预设 | 资产和要观察的现象 | 建议的同机位操作 |
| --- | --- | --- |
| `toon` | 银狼的二分光照、面部 SDF 和轮廓。屏幕边缘、法线外扩、边面邻接产生的四边形分别生成轮廓。 | `toon view face`、`toon sdf off`。全身视图用 `toon outline image`、`toon outline hull`、`toon outline quad`。 |
| `texture` | Modern Hall 的 Wood、FloorTiles、Wallpaper 使用特征网格和小型 MLP 还原基础色。 | `texture neural`、`texture original` 比画面，`texture stats` 看存储数据。 |
| `gaussian` | 默认使用 Modern Hall 的地砖，着色时查询打包的 2D Gaussian 表示。 | `gaussian scan`、`gaussian tile`、`gaussian bvh` 比查询。`gaussian compact-bvh` 和 `gaussian stats` 检查紧凑表示与索引成本。 |
| `gi` | 程序化红绿墙、白色测试块、球体及面光源，控制反弹遮挡与染色。 | 切换 `gi off`、`gi ssgi`、`gi probes`、`gi ray`。`gi view indirect` 只看间接项。 |
| `shadow` | 带网格标记的接收平面和遮挡体，便于观察接触处与远端半影。 | `shadow hard`、`shadow pcf`、`shadow pcss`、`shadow ray`。`shadow view visibility` 查看遮挡值。 |
| `rt` | 独立的 KHR 光追管线对程序化物体发射主射线，需要设备支持 `VK_KHR_ray_tracing_pipeline`。 | `rt view normals`、`rt view distance`、`rt view reflections`。 |
| `restir` | 程序化大厅、四个共享低面数阴影代理的球和多光源。 | `restir reuse`、`restir reference` 比采样。`restir denoise off\|spatial\|svgf` 选择直接光滤波，`restir gi direct\|uncached\|single\|multi` 选择漫反射间接光。 |
| `gi12-forest` | 在程序化植被上组合 ReSTIR 直接光和探针／缓存间接光，检查林地、树冠和阴影。 | `restir gi direct`、`restir gi single`、`restir gi multi`。`restir rays auto` 使用设备能力选择查询。 |
| `gi12-silver` | 动画银狼使用材质自定义命中着色和贴图 UV，检查非写实材质的间接光。 | 保持机位与姿态一致，使用同一组三种 `restir gi` 设置。 |
| `ibl` | HDR 摄影棚环境和程序化材质球，观察粗糙度、金属度与环境反射。 | `ibl view diffuse`、`ibl view specular`，再用 `ibl yaw 90` 旋转环境。 |
| `mixed` | 将 IBL 环境与 ReSTIR 大厅组合，环境与有限光源共享材质表面。 | 分别使用 `ibl ev -4` 和 `restir ev 0` 调整各自贡献。 |

光照方法、求交后端与降噪器由不同选项控制。`gi12-forest`、`gi12-silver` 是仓库对探针和辐亮度缓存的实验实现。重现具体结果时请记录场景、`restir gi`、`restir rays` 和降噪设置。[基础光照场景与模式](../src/experiments/lighting.rs) · [ReSTIR 场景与命令](../src/experiments/restir.rs) · [森林与银狼 GI 配置](../src/experiments/restir/studies.rs) · [IBL](../src/experiments/ibl.rs)

## 输入、捕获和资产

`--scene FILE.json` 恢复引擎存档。单独传 glTF 可导入默认世界。`--experiment clusters --scene FILE.gltf` 为导入几何准备分层簇。`--experiment toon --scene FILE.gltf` 为人物装配卡通材质。`--experiment gaussian --scene FILE.g2t` 在独立纹理预览上读取打包数据。其它具名预设由自己的构造函数提供场景。打包的 Dragon、银狼、Modern Hall 与环境贴图的署名、源文件和授权见[资产目录](../assets/README.md)。

截图可显式指定尺寸与渲染预热帧数。`--secondary-capture` 从同一次模拟读第二个相机，`--secondary-yaw` 单位为度。录制会按给定帧率推进模拟，`--at` 在相应模拟秒数执行控制台命令，MP4/GIF 需要 FFmpeg。窗口内可用 `record target/live.mp4`、`stoprecord` 记录交互。运行中的 RDG 可通过 `rdg` 查看，`timeline target/frame.json` 导出当前帧的 GPU 队列区间。[控制台](../src/console.rs) · [帧捕获与录制](../src/recording.rs)

```powershell
cargo run --release --locked -- --experiment clusters --command "lodview on" --capture target/clusters.png --size 1280x720
cargo run --release --locked -- --experiment silver-contact-duo --capture target/front.png --secondary-capture target/side.png --secondary-yaw 45
cargo run --release --locked -- --experiment motion --record target/motion.gif --fps 24 --duration 3 --at 1 "motion 1 0"
```

成品场景可直接运行。重训神经纹理时使用 PyTorch、NumPy 和 Pillow。动作 PCA/KD 索引可独立校验。Gaussian 工具支持拟合、PLY 打包和按 tile 尺寸构建索引。[神经纹理训练](../tools/train_texture_grid.py) · [Gaussian 打包](../tools/gaussian_texture.py) · [动作烘焙](../tools/motion_index.py)

```powershell
python tools/train_texture_grid.py assets/scenes/modern-hall/modern_hall_core.gltf target/modern-hall
python tools/motion_index.py --check
```
