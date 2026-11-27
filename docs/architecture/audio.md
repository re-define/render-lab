# 简单音频模块设计

第一版服务于二维短音效。实验或控制台在主线程加载声音、发起播放并取得一个播放 ID。场景可以在一次物理操作成功后发声，用户也可以单独试听。验收场景是 `movement` 中的刚体冲量。

## 所有者与时间

| 部分 | 职责与生命周期 |
| --- | --- |
| `AudioClip` | 保存已经解码的交错 PCM、声道数和采样率。读取 WAV 和校验在调用线程完成。一个片段可以重复播放，各次播放共享采样内存并拥有独立游标。 |
| `Engine.audio` | 管理主音量、暂停状态、播放 ID 及活动声音。首次需要发声时打开系统输出设备；同时播放上限为 16，达到上限时向调用者返回原因。 |
| 输出设备 | 使用 Rodio/CPAL 的混音与设备回调。设备按自己的采样时钟连续消费声音，并将源采样率转换成输出采样率。音频回调只消费声音，ECS、RDG 和 RHI 沿用原有线程边界。 |

世界 tick 决定事件何时发生。第一版在收到已确认的事件后尽快开始播放。窗口暂停按钮和控制台 `pause` 同时暂停模拟与在播声音，恢复时从原播放位置继续。成功替换场景会停止旧场景声音；加载失败保留原场景与播放状态。保存场景只保存模拟数据，临时试听和正在播放的游标属于运行会话。

## 接入与故障处理

主线程可以播放内置撞击音或单声道、立体声 WAV。加载器接受 8、16、24、32 位整数 PCM 和 32 位浮点 PCM，采样率上限 192 kHz。每段最多 8,388,608 个交错采样，解码后上限为 32 MiB。默认音量为 0.35。交互窗口中的非零刚体冲量操作成功后触发一次撞击音。离屏截图、视频录制与基准不自动发声，显式试听命令仍可使用。没有输出设备时启动、模拟和渲染均可继续，试听命令报告设备错误，用户可以在设备恢复后重试。

```text
audio status
audio play impact
audio play assets/audio/impact.wav
audio cues on
audio volume 0.2
audio stop 1
audio stop all
```

## 验收

从仓库根启动 `cargo run --release --locked -- --experiment movement`。打开控制台，输入 `object 6 impulse 0 2 0 at 3.5 2 0`，右侧刚体受到偏心冲量时会响一次。输入 `audio play impact` 可单独试听，`audio status` 显示设备格式、播放 ID 和音量。

用内存中的混音输出检查多个声音是否叠加、音量是否生效、暂停是否冻结游标、停止是否能区分播放 ID。用实际 WAV 覆盖解码、文件路径和非法格式。运行单独的输出设备测试，区分实际设备消费与无设备回归的结果。运行 `movement` 的真实命令入口，检查一次成功的冲量只发出一次播放请求。音轨封装、三维声源和流式音乐属于后续扩展。

```powershell
cargo test --release --locked --lib audio::tests
cargo test --release --locked --lib audio::tests -- --ignored --nocapture --test-threads=1
```

[播放管理](../../src/audio.rs) · [WAV 解码](../../src/audio/clip.rs) · [控制台](../../src/console/audio.rs) · [设备、场景与混音测试](../../src/audio/tests.rs)
