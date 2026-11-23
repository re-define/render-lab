# 实验资产来源

场景、人物与预训练数据均在本仓库的 `assets/` 中。运行命令以仓库根目录作为工作目录。

| 资产 | 仓库内入口 | 来源与许可 |
| --- | --- | --- |
| Dragon | `scenes/dragon/dragon_core.gltf` | Delatronic，Benedikt Bitterli Rendering Resources 场景转换为 glTF，CC BY 3.0。原文见场景目录内的 `LICENSE.txt` |
| Modern Hall | `scenes/modern-hall/modern_hall_core.gltf` | NewSee2l035，Benedikt Bitterli Rendering Resources 场景转换为 glTF，CC BY 3.0。原文见场景目录内的 `LICENSE.txt` |
| Cornell Box | `scenes/cornell-box/cornell_box_core.gltf` | Benedikt Bitterli，转换为 glTF，CC0。原文见场景目录内的 `LICENSE.txt` |
| Silver Wolf | `characters/silver-wolf/scene.gltf` | X9_YT，CC BY 4.0。模型署名与原文见人物目录内的 `license.txt` |
| Studio Small 02 | `environments/studio_small_02_1k.hdr` | Greg Zaal，Poly Haven，CC0。IBL 实验从线性 HDR 图生成环境光照表。[原始资产](https://polyhaven.com/a/studio_small_02) |
| Rock Face 03 | `environments/rock_face_03_diff_1k.jpg` | Dario Barresi 摄影，Rico Cilliers 处理，Poly Haven，CC0。山谷岩壁使用扫描反照率。[原始资产](https://polyhaven.com/a/rock_face_03) |
| Rocky Terrain | `environments/rocky_terrain_diff_1k.jpg` | Amal Kumar，Poly Haven，CC0。林间裸地使用扫描反照率。[原始资产](https://polyhaven.com/a/rocky_terrain) |

Dragon 和室内场景取自同一份 glTF Research Scenes 本地转换结果。随附 core glTF 及其引用的 buffer、纹理和许可文本。[Rendering Resources](https://benedikt-bitterli.me/resources/) · [Dragon 原作](https://blendswap.com/blend/15891) · [Modern Hall 原作](https://blendswap.com/blend/6304)

This work is based on "Star Rail - Silver Wolf" by X9_YT, licensed under CC BY 4.0. 人物用于卡通、骨骼重定向与动作匹配实验。[模型来源](https://sketchfab.com/3d-models/star-rail-silver-wolf-7fc76dd8fe5444ccb559cc497d4bfb23) · [作者](https://sketchfab.com/X9_YT) · [许可](https://creativecommons.org/licenses/by/4.0/)

`texture/modern-hall/` 的特征网格和 `gaussian/` 的高斯资产由 Modern Hall 基础色贴图拟合得到，沿用该源资产的署名与 CC BY 3.0 许可。剪枝、量化和训练过程见仓库文档。[纹理训练说明](../README.md#modern-hall-神经纹理压缩) · [高斯训练说明](../docs/gaussian-texture.md)

`motion/source/` 是 NVIDIA Kimodo 生成的 39 段 SOMA 动作；`motion/bank.json`、`poses.bin` 与 `search.json` 是银狼重定向和检索数据。模型 ID、版本和生成过程记在索引与仓库文档中。[Kimodo](https://github.com/nv-tlabs/kimodo) · [模型来源](https://huggingface.co/nvidia/Kimodo-SOMA-RP-v1.2)
