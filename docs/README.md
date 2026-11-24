# Render Lab 文档索引

从项目根目录执行文档中的命令。可用预设以 CLI 的 `--list-experiments` 输出为准。运行环境和首次启动见[项目 README](../README.md)。

## 运行实验与采集结果

| 用途 | 入口 |
| --- | --- |
| 查找实验、适用设备和兼容入口 | [实验目录](experiment-catalog.md) |
| 查找控制台命令、录制和窗口 benchmark 参数 | [实验手册](experiment-guide.md) |
| 检查交换链、鼠标、HUD、TAA 与 DLSS | [视口与后处理](viewport-post.md) |
| 配置灯光与组合实验 | [场景光源](scene-lights.md) · [场景组合](scene-composition.md) |

## 修改引擎

| 问题 | 契约与源码入口 |
| --- | --- |
| 模块如何协作，一帧由谁拥有 | [架构与帧流程](architecture.md) · [模块关系图](module-map.md) |
| 新系统如何声明组件、资源和实体姿态访问 | [运行时与场景](runtime-and-scene.md#模拟阶段如何读写) |
| 新 pass 如何声明资源，队列为何等待 | [渲染管线](render-pipeline.md#rdg-如何编译rhi-如何执行) · [森林队列时间线实测](forest-architecture-benchmark.md#rdg-屏障与跨队列交接) |
| 如何接入 KHR 管线、内联查询或 NV 簇几何 | [光追 RHI 能力矩阵](ray-tracing-rhi.md) |
| 提交前如何校验代码、资产和 GPU 路径 | [开发与验证规范](../CONTRIBUTING.md) |

## 虚幻引擎源码调研

| 主题 | 执行流程与算法 |
| --- | --- |
| 动画 | [Unreal Animation Framework](unreal-animation-framework.md) · Trait 与姿态求值、Pose Search、精确接触、骨骼重定向、物理反馈 |
| 物理 | [Chaos 物理与破碎](chaos-physics.md) · CCD、接触有效质量与摩擦、求解岛、破碎与回退 |

## 专题与测量

| 主题 | 构建过程、配置和历史数据 |
| --- | --- |
| 植被 LOD、体素构建和光栅 | [Nanite 与本引擎的源码对照](nanite-voxel-pipeline.md) · [程序化植被](procedural-vegetation.md) · [森林河流](river-forest.md) |
| 森林阴影与显存 | [软硬阴影](forest-shadows.md) · [Mega Geometry](forest-mega.md) · [BLAS、队列与帧时间](forest-architecture-benchmark.md) |
| 光照研究 | [GI、阴影、RT 和 IBL 对照](lighting-comparison.md) · [GI-1.2 与 ReSTIR DI](gi12.md) |
| ECS 编辑的开销 | [森林增量提取](ecs-changes.md) |
| 纹理压缩 | [二维高斯解码与显存](gaussian-texture.md) · [高斯裁剪与精度](gaussian-pruning.md) |

架构、运行时和渲染管线文档描述当前执行顺序。专题文档内的旧版本数据保留原始列名、场景参数和采样条件。
