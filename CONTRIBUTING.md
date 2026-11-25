# 修改和验证引擎

从仓库根目录执行命令。Rust 使用 edition 2024，公共类型写明所有者、坐标空间、时间单位和失败行为。系统输出与调试命令使用同一运行入口，以便存档、窗口、录制和测试复用。

| 修改位置 | 应核对的契约 |
| --- | --- |
| ECS 与任务 | 声明组件和资源读写，骨骼用具体实体 ID 绑定。并行任务读同一快照，按输入次序提交。失败时检查前序修改的回滚。改变绑定后，在同一固定子步的下一阶段重新解析。 |
| 场景提取 | 检查初建、稀疏修改、连续大量姿态写入、不同步的两个提取器、结构修改和失败恢复。用冷提取核对模型和骨骼矩阵。添加快照组件时声明依赖及监听的世界变换。 |
| 动画与物理 | 新输入通过固定时钟进入模拟。明确运动由哪个系统写回，给存档新增字段提供旧数据默认值。用不同显示步长、本地坐标和世界空间的断言复验。 |
| RDG、RHI 与 shader | 标明 pass 分类、真实资源读写、图像布局和队列。检查帧槽复用、跨队列等待、历史失效、资源退役，以及 Rust 和 shader 的数据布局。 |
| 实验、CLI 与资产 | 登记预设、系统、材质生产者和恢复入口。更新实验目录。导入或更换第三方资产时保存来源和许可。 |

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --release --locked --lib silver_crowd_extraction_profile -- --ignored --nocapture
cargo test --release --locked --lib silver_crowd_window_profile -- --ignored --nocapture
```

对不同提交做 Rust 与着色器的联合对照时，给两个 checkout 分别指定 Cargo 的 `--target-dir`，使每个可执行文件嵌入与该提交匹配的 SPIR-V。人群窗口基准同时记录提交人物和至少有一个可见像素的人物。

标为 ignored 的测试在各自源码中列出依赖，包括特定资产、Vulkan 设备、窗口或 NGX。运行图形集成时可启用 `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` 和 `VK_LAYER_VALIDATE_SYNC=1`。更改压缩工具后运行 `python -m unittest discover -s tools -p "test_*.py"`。提交数据应附构建模式、设备、输入、采样范围和统计口径。

文档用相对链接指向实现或测试。对研究原型明确写出简化条件，引用官方资料时区分预览特性与本仓库已经实现的功能。[架构](docs/architecture.md) · [GPU 管线](docs/rendering.md)


修改帧准入、录制和提交时，同一窗口基准应保留每个完成帧，区分提交编号、退役间隔和 GPU 跨度。修改蒙皮时比较当前和历史姿态的输出，保留对零权重顶点、多个角色、共享调色板、物理队列边界和 NGX 录制线程的覆盖。[帧生命周期](docs/architecture.md) · [性能测量](docs/performance.md)

更改角色求解时验证两角色共享平台的总冲量、不同线程数的结果、逐子树和全量 FK 等价，以及存档后重绑。更改材质组合时检查关闭不影响机位的层后线性 HDR 输出的全部像素。修改分页资产时分别覆盖不变版本、单页写入、两帧在途和突破容量的重新分配。[动画与物理](docs/animation-physics.md) · [渲染颜色](docs/rendering.md) · [纹理实测](docs/extraction.md)
