# 渲染接口、RDG 与 Vulkan 提交

渲染前端将可见性、材质和各个实验阶段翻译为带资源访问声明的 pass。RDG 编译逻辑依赖，RHI 结合物理队列与帧槽执行计划。阅读森林的三角簇、光照与像素结果可转到[可见性与光照](../algorithms/visibility-lighting.md)。

## 绑定预计算包围盒与阴影代理

一个普通刚体实例可引用共享的 `GeometryAssets`。其中的局部 AABB 必须包围源网格全部顶点；实例的模型矩阵负责变换包围盒。投影代理与视觉网格使用同一局部坐标。可见性与 GI 采用视觉网格，直接光任意命中可查询代理。没有单独代理时，两种用途可以共享加速数据。蒙皮、风动和簇几何按自身变形或层级处理边界和阴影。[绑定校验](../../src/scene/geometry_assets.rs) · [表面准备](../../src/render/geometry.rs)

前端区分 KHR 光追管线、内联 ray query、NV 簇加速结构三项设备能力。软件遍历读取 BVH 与三角流，硬件求交读取 BLAS 和 TLAS；同一帧内视觉与遮挡可以复用相同几何的 BLAS。静态几何可按版本跨帧槽共享，动态几何随姿态更新。森林启用 Mega Geometry 时还要把屏幕外可能投影进来的簇加入候选集。选择何种后端要结合设备能力、场景设置和实际 pass。[求交前端](../../src/render/rays/scene.rs) · [KHR 后端](../../src/rhi/ray.rs) · [森林簇光追](../../src/experiments/clusters/gpu/mega.rs)

## GPU 变形怎样进入通用蒙皮

普通角色从已发布的关节姿态生成矩阵页，毛发则由 GPU 变形阶段直接提供当前及可用的上一显示帧矩阵。同一表面的可见性、运动向量和后续光追几何都能沿用蒙皮输出。资源依赖使蒙皮等待其输入矩阵就绪。每个视图分别保存粒子历史；这类几何缓冲的大小由引导点数量决定，窗口变更分辨率仍可接续求解。重新绑定或重置发型时切换历史代次。[变形注册](../../src/experiments/mod.rs) · [毛发 pass](../../src/experiments/hair/gpu.rs) · [蒙皮批次](../../src/render/geometry/skin.rs)

## RDG、Vulkan 与呈现

1. **声明资源。** 每个 pass 指定操作、分类、队列及资源访问。缓冲可以声明字节区间，例如一个 Hi-Z mip；图像以整个资源追踪。普通 GPU 临时输出用 `Graph::local_buffer` 声明逻辑字节数和设备本地内存偏好。带初始内容的输入、历史和可复用 scratch 各有对应声明。[RDG](../../src/rdg.rs) · [区间访问](../../src/rdg/ranges.rs)
2. **编译依赖。** 编译器从资源读写、字节区间、图像布局和显式前驱生成顺序与屏障。主视图和次视图各保存最近一份拓扑。命中需要操作类型、队列、资源类型与初值状态、直接绑定、间接地址表和区间边界等调度输入一致。着色器、间接参数和加速结构输入仍用当帧命令校验。随后从当帧呈现、读回、历史写入和外部副作用追溯活跃 pass，再决定实际分配与提交。[调度模板](../../src/rdg/schedule_cache.rs) · [活跃性裁剪](../../src/rdg/liveness.rs)
3. **录制与提交。** RHI 使用退役帧槽准备资源和描述符堆，安排 worker 用独立的录制器录制命令。逻辑队列按设备配置映射到同一通用队列族的物理队列。同一队列内用屏障约束有依赖的阶段和内存访问。跨物理队列的依赖合并到时间线信号量，已被等待覆盖的普通缓冲读可以省去相应屏障。图像布局转换、主机可见性和别名边界仍有各自的同步要求。[帧槽](../../src/rhi/slot.rs) · [录制](../../src/rhi/record.rs) · [队列与等待](../../src/rhi/queues.rs)
4. **显示与读回。** 窗口帧在 GPU 端写入交换链呈现，优先 MAILBOX，设备不支持该模式时使用 FIFO。离屏输出、截图和录像按需读回。比较异步调度应使用对齐后的队列时间线、跨队列等待和整帧跨度；相互重叠的 pass 时长直接相加会重复计算。[交换链](../../src/rhi/present.rs) · [GPU 时间线](../../src/rhi/timeline.rs) · [性能与复现](performance.md)

### 调度策略与复测

默认优先延续上一逻辑队列。设置 `RENDER_LAB_RDG_ORDER=critical` 会优先调度下游依赖链较长的就绪 pass，链长按 pass 个数计算。物理队列映射由 RHI 在裁剪之后决定。完全串行的图可以使用一条队列，另外保留 RTX 5070 上经过 1080p 森林回放校准的组合。设置 `RENDER_LAB_GPU_QUEUES=1|2|3` 可以固定同机 A/B 所用队列数。设置 `RENDER_LAB_RDG_PLAN_CACHE=0` 可以单独关闭调度模板。基准逐帧记录构图、依赖编译、缓存命中、映射可用队列数、实际收到提交的队列数、时间线等待和 GPU 跨度。[物理队列映射](../../src/rhi/queues.rs) · [森林镜头回放](../../examples/forest_shadows/zoom.rs)

## 验证入口

以下测试需要 Vulkan 设备。第一项在同一机位比较视觉 ID、代理阴影与 GI 几何，覆盖软件 BVH、KHR 管线和内联查询；第二项用相同模拟输入核对两个相机各自的 TAA 历史。普通网格合批的逐像素 ID、深度与运动对照，以及森林体素与 GPU 性能的运行条件，分别见[普通绘制回归](../../src/experiments/motion/silver/contact/crowd_draw_test.rs)和[性能记录](performance.md)。

```powershell
cargo test --release --locked --lib rigid_derived_assets_change_shadows_but_preserve_visual_and_gi_geometry -- --ignored --nocapture
cargo test --release --locked --test offscreen_views -- --ignored
```
