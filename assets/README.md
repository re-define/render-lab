# 资产和衍生数据

程序从仓库根解析资产路径。几何、材质和纹理在导入后映射到引擎实体，新导入数据应保留原始署名与许可文件。

| 路径 | 原作、许可与用途 |
| --- | --- |
| [Dragon](scenes/dragon/dragon_core.gltf) | Delatronic，CC BY 3.0。用于层级簇、可见性和普通几何对照。[原作](https://blendswap.com/blend/15891) |
| [Modern Hall](scenes/modern-hall/modern_hall_core.gltf) | NewSee2l035，CC BY 3.0。提供室内场景以及神经网格与 Gaussian 拟合的纹理来源；衍生数据沿用署名和许可。[原作](https://blendswap.com/blend/6304) |
| [Cornell Box](scenes/cornell-box/cornell_box_core.gltf) | Benedikt Bitterli，CC0。上述场景基于 Rendering Resources 的 glTF 转换，随附原始许可。[场景集](https://benedikt-bitterli.me/resources/) |
| [银狼](characters/silver-wolf/scene.gltf) | 基于 X9_YT 的 “Star Rail - Silver Wolf”，CC BY 4.0，用于着色、重定向、蒙皮和接触测试。[原作](https://sketchfab.com/3d-models/star-rail-silver-wolf-7fc76dd8fe5444ccb559cc497d4bfb23) · [作者](https://sketchfab.com/X9_YT) · [许可](https://creativecommons.org/licenses/by/4.0/) |
| [Studio Small 02](environments/studio_small_02_1k.hdr) | Greg Zaal，Poly Haven，CC0。用作 IBL 的环境光照输入。[来源](https://polyhaven.com/a/studio_small_02) |
| [Rock Face 03](environments/rock_face_03_diff_1k.jpg) | Dario Barresi 摄影，Rico Cilliers 处理，Poly Haven，CC0。用于岩壁反照率。[来源](https://polyhaven.com/a/rock_face_03) |
| [Rocky Terrain](environments/rocky_terrain_diff_1k.jpg) | Amal Kumar，Poly Haven，CC0。用于林地反照率。[来源](https://polyhaven.com/a/rocky_terrain) |

`motion/source` 保存 39 段 SOMA 源动作。`motion/bank.json`、`poses.bin`、`search.json` 用于银狼检索和重定向，`motion/traversal` 存放另行生成的跳跃、翻越和钻行动作。生成器使用 NVIDIA Kimodo-SOMA-RP-v1.2。[项目](https://github.com/nv-tlabs/kimodo) · [模型说明及许可](https://huggingface.co/nvidia/Kimodo-SOMA-RP-v1.2) · [引擎中的使用方式](../docs/animation-physics.md)
