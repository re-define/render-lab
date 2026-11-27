# 修改与验证

在仓库根目录工作，使用 Rust 2024 和仓库的 `Cargo.lock`。场景数据、CLI、窗口和捕获共用运行入口。变更公开接口时一并检查现有调用、旧存档和失败后的状态。[运行说明](README.md) · [源码架构](docs/architecture/README.md)

## 按修改范围选择验证

| 修改范围 | 需要保住的行为 |
| --- | --- |
| ECS、系统调度与时钟 | 声明组件、资源和变换读写范围。按实体绑定依赖，验证一与多 worker 的结果、失败回滚和绑定变更。存档保留稳定 ID、时钟频率和剩余时间。 |
| Move、动画与物理 | 声明运动写入者、坐标空间与单位。同一子步有反馈的阶段共用时钟。检查运动学与动力学交接、共享支撑、IK 和存档重绑。 |
| 场景提取、资产与蒙皮 | 比较增量提取与冷提取的矩阵和调色板。覆盖无变化、单实例、共享资产、多读者和结构变更。分开验证当前姿态与前一帧姿态。 |
| 渲染、RDG、RHI 与 shader | 明确资源读写、图像布局、队列和可观察输出。验证帧槽复用、跨队列等待、历史失效和资源退役。GPU 的像素或缓冲断言应说明格式、设备与所需扩展。 |
| 实验与工具 | 登记预设、系统、材质或主技术，验证新建、保存后重载及支持的运行时开关。CLI 和窗口使用相同的行为路径。新资产保留作者署名和许可。 |

## 常用命令

```powershell
cargo fmt --all -- --check
cargo clippy --release --locked --all-targets -- -D warnings
cargo test --release --locked --all-targets
python -m unittest discover -s tools -p "test_*.py"
```

Python 烘焙和压缩工具测试需要 NumPy 与 plyfile，编码测试使用 FFmpeg。有额外硬件、资产或时间开销的用例在源码标为 `ignored`。例如，验证两个相机的时域历史可运行下列设备测试。[双视图回归](tests/offscreen_views.rs) · [编码回归](tests/recording.rs)

```powershell
cargo test --release --locked --test offscreen_views -- --ignored --nocapture
```

## 文档和测量

架构文档交代模块所有者、输入输出、时间与资源生命周期；算法文档交代输入、计算步骤、误差、限制及复现方法。与虚幻引擎的差异和借鉴单列，写明 UE 文档版本、本仓库的实现入口，以及提案的验收条件。改动实验名或调试语法时同步修改实验目录。用仓库相对链接指向源码、测试与原始数据；第三方许可文件随原资产保存。已经完成的迁移过程由 Git 记录，现行手册描述当前行为。[文档索引](docs/README.md) · [实验目录](docs/experiment-catalog.md) · [资产署名](assets/README.md)

比较两个提交时分开 Cargo target 目录，防止不同版本嵌入相同的 SPIR-V。记录提交、硬件、分辨率、渲染设置、预热与样本数。分别报告模拟、提取、GPU 时间跨度和窗口完成间隔；并行 pass 各自计时可能重叠。测量结论与复现命令集中到性能文档。[性能与复现](docs/architecture/performance.md)
