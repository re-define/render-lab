# 河谷密林与草地视觉实验

固定种子生成 22,750 棵树和 256,081 丛草。每棵树由共享的枝干和树冠两部分组成。每丛草有五个分蘖中心、45 片基础叶，全场共 11,523,645 片基础草叶，草穗另计。树草、水面和地面合计 301,583 个渲染实例。河道在谷地方向跨越 600 米，沿弯曲中心线约 655 米，上下游水位落差为 8.4 米。 [河道视角](../captures/river-forest-river.png) · [流域俯视](../captures/river-forest-basin.png) · [8 秒实录](../captures/river-forest.mp4)

~~~powershell
cargo run --release --locked -- --experiment river-forest --command "post taa"
~~~

控制台可用 `vegetation camera river` 回到河面，`vegetation camera grass` 检查岸边草根，`vegetation camera glade` 查看带穗草地，`vegetation camera basin` 查看森林的完整密度。`vegetation wind 0` 固定风姿，`effect time 1.25` 单独设置流水时间，`vegetation view paths` 查看植株的三角簇与体素簇。

## 草地制作依据

| 观察 | 在场景中的处理 |
| --- | --- |
| 真实丛生草可以同时具有密集细叶和疏松的圆锥花序。湿生莎草可在水岸形成不同于旱地草甸的草丛。 | 开阔处的小穗着生在纤细的分枝上。湿岸的莎草叶片较高、向外弓曲，脚下混生较矮的草。 |
| Sucker Punch 的 GDC 演讲将草丛作为高度和朝向的控制单位，沿弯曲叶片计算法线。 | 将密度、高度和主朝向绑定到米级斑块，每丛内部再改变叶长、弯曲程度和叶宽，叶片截面保留中脊。 |
| Epic 的 Landscape Grass 工作流程允许不同地表层使用不同草型和密度。 | 距离水线、树冠遮蔽和林间空地共同决定三种草型的分布。日照等级共用网格，调整材质色调。 |

实物形态来自北卡州立大学推广资料，分布与造型采用程序化近似。 [丛生发草](https://plants.ces.ncsu.edu/plants/deschampsia-cespitosa/) · [湿生莎草](https://plants.ces.ncsu.edu/plants/carex-elata/) · [Sucker Punch 原讲稿](https://archive.thedatadungeon.com/ghost_of_tsushima_2020/documents/gdc_2021/gdc_2021_procedural_grass_in_got.pdf) · [Epic Landscape Grass](https://dev.epicgames.com/documentation/en-us/unreal-engine/grass-quick-start-in-unreal-engine)

草根按渲染地形三角形的实际交点和坡面法线摆放。叶根较暗，叶尖透光，近景按太阳方向计算屏幕空间接触阴影。远处沿用引擎的混合几何 DAG 和面积补偿。使用现有风骨骼将相邻草丛的摆动关联起来，前后帧共享位移供 TAA 使用。 [两种生境的近景对照](../captures/river-forest-grass-study.png) · [草叶和穗形](../src/experiments/vegetation/generate/grassland.rs) · [植株分布](../src/experiments/vegetation/river_forest.rs)

## 河道与绘制

河床、水面与排除种植区域共用中心线和宽度函数。地形网格在水线两侧加密，以减少狭窄河段出现干缝的现象。GPU 每帧更新 33 × 1,201 个水面顶点、波纹和法线，使用解析波形和顺流输运描述水流。河中央的输运速度为 1.71 米/秒，外缘为 1.10 米/秒。时间重投影沿同一条水道回溯上一帧位置，新流入的水团丢弃历史。当前模型适用于固定河槽，水深和流量由场景预设，反射取样范围限于屏幕中可见的景物。 [河槽和 RDG](../src/experiments/effects/river.rs) · [水面计算](../shaders/river_surface.comp) · [河岸倒影](../shaders/river_reflections.comp)

在 800 × 500 固定河道机位关闭风与时域后处理，分别把流水时钟设置为 0 秒和 1.25 秒。两次截帧均使用相同的引擎预热。中央水面选区的 9,425 个像素中有 9,288 个发生变化，左上角 190 × 35 的天空选区逐像素相同。GPU 测试另外验证了河中央及两侧水团的回溯位置、模型变换和流入边界。

## 实测

RTX 5070、Release、816 × 559、MAILBOX、TAA、默认风动。每个固定机位预热 60 帧，测量 48 帧。GPU 帧耗时取设备时间戳，窗口帧间隔按完成呈现请求的时间统计。两种机位的 RHI 设备本地分配约 2.54 GiB，含簇工作列表、风骨骼、场景表及渲染目标。 [四组测量数据](../captures/river-forest-benchmark.csv)

| 机位 | GPU 中位数 | GPU P95 | 窗口帧间隔 P95 | 每秒呈现请求 |
| --- | ---: | ---: | ---: | ---: |
| 河道 | 6.82 ms | 7.11 ms | 31.84 ms | 32.2 |
| 流域俯视 | 11.78 ms | 12.19 ms | 32.09 ms | 32.2 |

场景含约 5,203 万个潜在实例簇，现有剔除工作列表按这一上限预留容量。UI 查询银狼及调试组件时使用按组件名建立的实体索引。创建或删除组件、更换整行以及加载场景后会刷新索引。更新已有风与流水时钟值时保留索引。同一场景使用旧的逐实体扫描时，河道和俯视机位的窗口帧间隔 P95 分别为 34.49 ms 和 32.29 ms。 [组件索引与更新规则](../src/scene/changes.rs)
