# Render Lab

Render Lab 是用 Rust 编写的 Windows / Vulkan 1.4 实验引擎。可以用同一套角色、材质和相机比较几何、光照、动画、物理和纹理压缩算法。银狼障碍场演示了动作匹配、固定步长动力学、手脚 IK 与 GPU 蒙皮如何一起工作。岩柱森林把簇几何、体素、风动、重采样光照和流水放进同一视图。

## 运行

在仓库根目录使用 MSVC Rust 工具链，安装 Vulkan SDK 并让 `glslc` 位于 PATH。设备需要 Vulkan 1.4、描述符堆扩展、mesh shader、动态渲染、同步 2、时间线信号量与交换链。光追扩展和 NGX 能力由设备初始化另行探测。[构建脚本](build.rs) · [设备选择](src/rhi/device.rs)

```powershell
cargo run --release --locked -- --list-experiments
cargo run --release --locked -- --experiment silver-contact
cargo run --release --locked -- --experiment forest-karst
cargo run --release --locked -- --experiment clusters --command "lodview on"
```

WASD 平移相机，视口内按住右键转向，F 重新取景。控制台输入 `help` 查看参数，`schedule` 查看模拟依赖，`rdg` 查看渲染图。用 `--benchmark 240` 对窗口呈现采样。用 `--capture target/frame.png` 保存一帧。安装 FFmpeg 后，`--record target/run.mp4 --size 1920x1080 --fps 60 --duration 10` 按指定时间推进并录制。录像时长与窗口吞吐量的测量方法见[性能与复现](docs/performance.md)。

[文档索引](docs/README.md) · [全部实验](docs/experiment-catalog.md) · [开发和测试](CONTRIBUTING.md) · [资产署名](assets/README.md) · [银狼实机录像](captures/silver-contact.mp4)
