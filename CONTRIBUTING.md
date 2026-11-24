# Render Lab 开发规范

从引擎根目录运行本文中的命令。项目入口和实验命令见 [README](README.md)，模块职责和依赖见[全引擎模块图](docs/module-map.md)。

## 代码放在哪里

| 改动 | 归属与约定 |
| --- | --- |
| 场景数据与模拟 | `scene` 拥有可保存的实体、组件和资产 ID。`engine::Schedule` 内的系统并行读同一阶段的 `World`，返回的 `Edit` 按注册顺序提交。需要固定步长的模拟使用 `fixed_stage`。 |
| 渲染前端 | `render::SceneExtractor` 从 ECS 提取只读快照。材质、几何和灯光程序从快照获取输入。持久数据使用实体或资产 ID，GPU 槽号限于生成它的注册表或帧。 |
| 实验 | 在 `experiments` 下按研究对象组织。场景构建、控制命令与 GPU 算法各自有明确入口。接入时同时考虑首次创建和从存档恢复。优先复用已有的几何准备、材质注册和后处理入口。 |
| GPU 资源与同步 | 语义 pass 使用 `Graph::pass_in` 指定稳定统计分类。pass 在 RDG 中声明读写、图像用途、地址表引用和历史资源。RHI 负责 Vulkan 对象、录制、队列同步与资源退役。修改共享资源生命周期时，用至少两个在途帧验证。 |
| 界面与工具 | UI 负责输入和显示，操作通过与 CLI 共用的控制台命令或引擎接口提交。训练、烘焙和格式转换放在 `tools/`，产物通过已有的导入或实验入口接入。 |

## Rust、shader 与数据格式

- 使用 Rust 2024 和 `rustfmt`。模块内部优先私有可见性，通过模块入口重导出对外类型。重构公共 API 时核对 CLI、库调用者、示例、测试和存档恢复路径。
- 按职责拆分文件。重复实现具有相同单位、所有权和失败语义时再合并。为类型、字段和函数命名时明确空间与单位，例如渲染像素和输出像素、模型空间和世界空间。注释解释约定、算法前提和生命周期。
- 外部文件、用户命令、设备能力与 SDK 失败通过 `Result` 传播，并注明资源或操作。导入和读档先构造、验证候选数据，成功后替换活动场景。为历史场景保留已有迁移逻辑。
- Rust 和 GLSL 共用的结构明确记录布局、对齐、索引和标志位。修改描述符顺序、push 常量、历史缓冲或纹理编码时，同时检查写入端和全部读取端。每个 shader 特化在 `build.rs` 中集中声明输出名、源文件和宏。
- Vulkan、Win32 和 C++ FFI 的 `unsafe` 边界注明调用者负责的对象有效期及同步条件。录制 worker 使用独立录制资源。帧时间、屏障数、队列等待数保持各自的统计口径。
- 清理代码时同时核对条件编译、SPIR-V 特化、序列化字段、插件接口与实验参考算法。例如动作库的全量扫描用于验证 KD-tree，`ngx` 的 SDK 分支按构建环境启用。

## 文档怎么维护

- 命名预设在 [CLI 清单](src/presets.rs)登记。新场景在[实验目录](docs/experiment-catalog.md)说明其独立验证目标。同一建场函数的算法组合优先列出控制台复现命令。移除预设时核对旧命令、存档加载、对照工具和专题文档。
- 改变光追契约时更新[光追 RHI 能力矩阵](docs/ray-tracing-rhi.md)。分别注明图编译能够验证的错误、依赖设备属性的限制，以及验证过的驱动与场景。
- README 负责启动、环境与实验索引。[模块图](docs/module-map.md)负责所有权、输入输出和源码入口。[架构详解](docs/architecture.md)记录端到端帧流程与设计取舍。线程和场景契约更新到[运行时与场景](docs/runtime-and-scene.md)，GPU 数据和同步契约更新到[渲染管线](docs/render-pipeline.md)。增删 CLI 预设时同步[实验目录](docs/experiment-catalog.md)。专题文档保存实验配置、结果和限制，其余位置链接到对应章节。
- 面向使用者的文字用中文，标识符、格式名和控制台命令保留源码拼写。仓库文件用相对链接。命令写明工作目录、必要资产或可选 SDK，示例采用可替换的参数或环境变量。
- 架构图的箭头注明传递的数据或调用目的。区分帧内依赖、跨帧历史、可选后端和离线工具。新增或移动模块时同步更新模块图与源码链接。
- 报告性能时给出设备、构建模式、场景、分辨率、相机、采样方法，以及呈现或回读路径。显存注明是否包含加速结构、缓存和帧槽。为历史基线保留测量条件，单独标明当前已验证的功能和实验限制。

## 验证与交付

日常改动从格式、静态检查和现有测试开始。新增测试优先覆盖这次改动能引起的具体回归。

~~~powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
~~~

涉及 RDG、shader、RHI、呈现或录制时，在具备项目所需 Vulkan 扩展、窗口环境、测试资产和 FFmpeg 的机器上运行相应集成测试。下面的命令串行运行本构建包含的全部 Rust 测试，包括通常跳过的 GPU 实验。

~~~powershell
$env:VK_INSTANCE_LAYERS = "VK_LAYER_KHRONOS_validation"
$env:VK_LAYER_VALIDATE_SYNC = "1"
cargo test --locked --all-targets -- --include-ignored --test-threads=1
~~~

涉及高斯打包格式时，在含 NumPy、plyfile 的 Python 环境运行 `python -m unittest discover -s tools -p "test_*.py"`。SDK 功能单独按[后处理文档](docs/viewport-post.md)验证。提交说明写清验证过的路径、已知限制和行为变化。
