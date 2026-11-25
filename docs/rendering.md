# 从几何到呈现

## 可见性和几何

渲染线程接收不可变快照。几何路由根据资产是否带簇层级、蒙皮和轮廓需求决定可使用的准备器。普通网格与簇网格都能写入主视图的可见性和深度；蒙皮 compute 读取骨骼调色板、权重及绑定姿态顶点，生成本帧表面。需要上一帧位置时，另一路 GPU 蒙皮使用上一已渲染姿态。材质解析使用像素所属表面的实例、三角形及材质索引进行分箱，分别运行相应的材质程序。独立主射线或纹理研究预设可以提供专用渲染技术。[几何与变形](../src/render/geometry.rs) · [可见性](../src/render/visibility.rs) · [材质表](../src/render/materials.rs)

植物的三角簇和体素砖参与同一层级构建；节点记录空间范围和选层误差。视锥筛选后，GPU 遍历候选节点和簇，结合屏幕误差、遮挡和前帧 Hi-Z 选择集合。首次光栅后建立中间深度层级，复查可能被错剔的三角簇。末尾的体素光栅消费压紧的砖列表，测试砖内占用和像素深度。比较轮廓、遮挡与 overdraw 时应固定风相位、相机和选层阈值。[构建与砖编码](../src/experiments/clusters/bvh/voxels.rs) · [簇调度](../src/experiments/clusters/gpu.rs) · [植被建场](../src/experiments/vegetation.rs)

## 光照、射线与时域数据

ReSTIR DI 从光源集合产生重要性采样候选，在配置允许时合并时空邻域的储层，然后针对选中的样本求遮挡。间接光路径采集探针或缓存的辐射亮度；GI 最近命中使用命中材质定义和插值 UV。IBL 为 PBR 提供环境漫反射和预滤镜面数据。SVGF 利用运动、深度、法线和历史统计复用光照，再执行空间滤波。[ReSTIR](../src/experiments/restir.rs) · [间接漫反射](../src/experiments/restir/diffuse_gi.rs) · [IBL](../src/experiments/ibl.rs)

前端分别查询 KHR 光追管线、内联 ray query 与 NV 簇加速结构能力。静态 BLAS 可按几何版本跨帧槽共享，动态几何随姿态更新。硬件射线消费 TLAS、命中表与材质属性；软件路径用计算着色器遍历 BVH。森林的簇光追额外处理屏幕外可投影的候选与风动姿态。支持某扩展和某实验实际选中该后端是两个不同的指标，应同时检查场景设置和 pass 报告。[射线前端](../src/render/rays.rs) · [KHR 后端](../src/rhi/ray.rs) · [NV 簇](../src/rhi/clusters.rs)

刚体、骨骼和过程变形的运动输入从渲染历史获得。TAA、DLSS 和时域光照共享屏幕运动，但分别持有自己的历史与有效性条件；相机剪切、尺寸变化和切换模式会影响这些历史。后处理前端声明需要的颜色、深度与运动，后端产生最终颜色。NGX 是可选本机集成，构建时由 `RENDER_LAB_NGX_SDK`、Vulkan 头文件和运行时 DLL 决定可用性。[运动输入](../src/render/motion.rs) · [后处理](../src/render/post.rs) · [NGX 构建](../build.rs)

## RDG 和 RHI

1. 每个 pass 声明操作、分类和资源访问。编译器跟踪读写依赖、布局变化和历史资源。可共用一次同步的访问被合并。逻辑队列映射到设备可用的物理队列；跨物理队列依赖生成时间线等待，同一队列按提交次序执行。[RDG](../src/rdg.rs) · [帧图](../src/render/frame_graph.rs)
2. RHI 为当前帧槽准备持久资源、瞬态缓冲和描述符堆，通过独立录制器把计划分发给 worker。队列提交信号和等待经合并后发出。资源按用途、生命周期和内存类型分配；复用和退役以相关时间线完成为条件。[RHI 执行](../src/rhi/mod.rs) · [内存分配](../src/rhi/allocator.rs) · [瞬态分配](../src/rhi/transient.rs)
3. 窗口使用 Vulkan 交换链，优先 MAILBOX，并依据设备可用模式选择。取得图像与呈现使用 WSI 信号量。GPUI HUD 由桌面合成叠在视口之上。离屏捕获与视频编码显式读回 GPU 图像。[呈现](../src/rhi/present.rs) · [HUD](../src/ui/hud.rs) · [编码](../src/recording.rs)

用 `timeline target/frame.json` 导出最近帧的物理队列与等待来源。整帧 GPU 跨度取公共时间域内的最早开始和最晚结束；单 pass 时长之和可能包含重叠。CPU 的 `rhi_ms` 从准备持续到退役，也可能跨越其他帧。对照 CPU 占用、帧完成间隔和 GPU 跨度时请注明各自口径。[帧报告](../src/rhi/frame.rs) · [时间线导出](../src/rhi/timeline.rs)
