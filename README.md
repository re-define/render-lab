# Render Lab

用于实时渲染、动画与物理研究的 Rust 引擎，运行于 Windows x64 和 Vulkan 1.4。可以在银狼障碍场研究动作匹配与接触 IK，在岩柱森林研究三角簇、体素和硬件光追，也可以将流水、角色和不同照明技术装配到同一个场景。[实验目录](docs/experiment-catalog.md) · [引擎架构图](docs/architecture.md)

## 构建与运行

在仓库根目录使用 x64 MSVC Rust 工具链，安装 Vulkan SDK，使 `glslc` 位于 PATH，或用 `GLSLC` 指定编译器。图形设备需要 Vulkan 1.4、`VK_EXT_descriptor_heap`、mesh shader、缓冲设备地址、时间线信号量、动态渲染与同步 2；窗口使用交换链。后端单独检测光追管线、内联查询和 NV 簇加速。DLSS/DLAA 另外需要在构建时配置 NGX SDK，并在运行时通过能力检测。[构建脚本](build.rs) · [设备选择](src/rhi/device.rs)

```powershell
cargo run --release --locked -- --list-experiments
cargo run --release --locked -- --experiment movement
cargo run --release --locked -- --experiment silver-contact
cargo run --release --locked -- --experiment forest-karst
```

`movement` 用同一张网格展示固定、运动学与受重力驱动的物体。控制台输入 `object` 查看实体，`tick` 查看模块时钟，`tick movement 30` 可现场改变运动频率。WASD 移动相机，视口内按住鼠标右键转向，F 重新取景。按 Enter 或 / 打开控制台，输入 `help` 查看命令，`schedule` 查看系统调度，`rdg` 切换渲染图面板。[世界与运动](docs/world-runtime.md) · [输入实现](src/ui/input.rs) · [控制台](src/console.rs)

## 捕获与测量

```powershell
cargo run --release --locked -- --experiment movement --capture target/movement.png --size 640x360
cargo run --release --locked -- --experiment silver-contact --record target/contact.mp4 --fps 30 --duration 5
cargo run --release --locked -- --experiment forest-karst --benchmark 240
```

录像需要 PATH 中的 FFmpeg，或通过 `RENDER_LAB_FFMPEG` 指定。离屏录像按固定时间推进；窗口基准记录完成的帧。两种运行方式的数值应分别解读。[性能与复现](docs/performance.md) · [已有银狼录像](captures/silver-contact.mp4)

[文档索引](docs/README.md) · [开发与验证](CONTRIBUTING.md) · [第三方资产与许可](assets/README.md)
