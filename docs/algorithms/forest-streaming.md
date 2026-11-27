# 森林树皮与树冠流式加载

`forest-stream` 从森林预设的同一个种子生成橡树，使用原有簇简化器与植被面积修正构建树冠三角 DAG。两个相邻分区各放置 12 个实例，共享一份树皮、树冠和页池。[实验入口](../../src/experiments/vegetation/streaming.rs) · [资产构建](../../src/experiments/vegetation/streaming/cook.rs)

## 资产与帧图

- **树皮**沿用普通橡树的树皮程序公式，把环绕树干的 U 与沿枝干的高度 V 烘成 512×512 纹理。每个 128×128 块独立寻址，带一份 8×8 区域平均根纹理。可见性缓冲保存实际命中的材质、静态光照系数及 UV。着色阶段根据相邻像素的 UV 足迹请求命中的块，同时读取本帧地址表指定的可用级。[原材质](../../shaders/vegetation_material.comp) · [按可见 UV 请求与着色](../../shaders/forest_stream_resolve.comp)
- **树冠**按可独立替换的简化组打包，一个逻辑组可以跨多个 64 KiB 物理页。根组及树干始终能画。元数据为每个组保留所有指向它的粗级父组、误差球和几何块。每帧由 GPU 按拓扑层次从粗到细计算切分。当所有父组都在当前切分路径上，且投影误差需要细化时，发出该组的请求。只有整个组就绪才让它参与绘制，同时排除粗级中指向它的全部绘制块。一个子组可被多个父组引用，文件目录载入时也会校验每条入边和每个几何范围。入选绘制块按全局编号压紧，每 4,096 块分配一条 mesh indirect 命令，只有首条清空可见性与深度；后续命令继续绘制同一幅图。[组选择](../../shaders/forest_stream_cut.comp) · [绘制块选择](../../shaders/forest_stream_draws.comp) · [网格着色器](../../shaders/forest_stream.mesh)
- **世界分区**按资产键 `forest.oak.v1` 合并需求。西区从根级即可激活，东区要求第一圈 4 个细树皮块就绪。即使世界里暂时还没有树，渲染线程也会提交必要页的传输，将可用等级回报给世界，再由主线程接入实体。退出最后一个有需求的分区时释放管理器；旧帧与上传提交按时间线退役。[分区运行时](../../src/scene/partition.rs) · [页管理](../../src/streaming.rs)
- 该实验聚焦静态树冠三角 DAG 和树皮，其余森林技术仍可在原来的森林预设中单独研究。植被体素砖、风动、阴影和 GI 在这条回放中没有产生额外请求。粗级几何及小型目录的 CPU 副本在场景存续期间保留，细级几何和纹理从磁盘包按需读取。

## 复现

```powershell
cargo run --release --locked -- --experiment forest-stream
cargo run --release --locked --example forest_stream -- target/forest-stream-study
```

回放使用正常的引擎 tick 与离屏帧完成接口，在同一设备上按 0、75、75、0 帧的保留期交叉回放，比较立即驱逐与短期保留。每次从空的引擎页池开始，操作系统文件缓存和驱动缓存可能已热。960×540 相机路径依次推近东区、三次转身折返、进入两区交叠处、移到西区，最后瞬移到两区以外。基准断言冷启动在零实例时已发起读盘、双区确实同时激活、单区离开后另一组仍绘制，以及末帧细页、承诺块、根页、上传暂存和待退役字节均为零。[回放与逐帧字段](../../examples/forest_stream.rs)

`gpu_ms` 是该帧 GPU 时间戳跨度。`tick_to_completion_ms` 包含提取、队列等待和离屏读回。首次可用 P95 从引擎收到 GPU 需求到某帧首次选择该级别。重复读取字节按实际页载荷计算，尾页按有效长度计数。这些指标不等于交换链窗口帧率或 SSD 冷缓存带宽。


RTX 5070 上四轮共 1,040 帧均覆盖两区的 24 个实例。两种策略均完成了树皮细块采样、多条非空 mesh 间接命令和最后一格卸载。[逐帧测量](../architecture/measurements/forest-stream.csv) · [近景截图](../../captures/forest-stream-near.png) · [性能口径](../architecture/performance.md)
