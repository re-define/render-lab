"""Retarget Kimodo SOMA world rotations to Silver Wolf and bake a 30 Hz search bank."""

import argparse
import json
from pathlib import Path

import numpy as np
from motion_index import write_index
from scipy.ndimage import gaussian_filter1d, median_filter
from scipy.spatial.transform import Rotation

PAIRS = {
    "Hips": "Hips",
    "Spine": "Spine2",
    "Chest": "Chest",
    "Neck": "Neck1",
    "Head": "Head",
}
for side in ("Left", "Right"):
    for target, source in (
        ("shoulder", "Shoulder"),
        ("arm", "Arm"),
        ("elbow", "ForeArm"),
        ("wrist", "Hand"),
        ("leg", "Leg"),
        ("knee", "Shin"),
        ("ankle", "Foot"),
        ("toe", "ToeBase"),
    ):
        PAIRS[f"{side} {target}"] = side + source


def unit(x):
    return x / np.linalg.norm(x)


def align(a, b):
    a, b = unit(a), unit(b)
    axis = np.cross(a, b)
    if np.linalg.norm(axis) < 1e-7:
        return (
            Rotation.identity()
            if a @ b > 0
            else Rotation.from_rotvec(np.pi * unit(np.cross(a, [0, 0, 1])))
        )
    return Rotation.from_rotvec(unit(axis) * np.arctan2(np.linalg.norm(axis), a @ b))


def smooth_rotation(rotation, sigma):
    q = rotation.as_quat()
    signs = np.where((q[1:] * q[:-1]).sum(axis=1) < 0, -1.0, 1.0)
    q[1:] *= np.cumprod(signs)[:, None]
    return Rotation.from_quat(gaussian_filter1d(q, sigma, axis=0, mode="nearest"))


def relaxed_arms(rest, parents, names, running):
    """Reference arm pose in each target joint's local space."""
    neutral = {}
    for side, sign in (("Left", 1), ("Right", -1)):
        shoulder, arm, elbow, wrist = [
            names[f"{side} {joint}"] for joint in ("shoulder", "arm", "elbow", "wrist")
        ]
        upper = rest[elbow][:3, 3] - rest[arm][:3, 3]
        lower = rest[wrist][:3, 3] - rest[elbow][:3, 3]
        upper_swing = align(upper, [sign * 0.20, -0.97, 0.10])
        lower_goal = (
            [sign * 0.14, -0.28, 0.95] if running else [sign * 0.15, -0.94, 0.30]
        )
        lower_swing = align(upper_swing.apply(lower), lower_goal) * upper_swing
        rotations = {
            shoulder: Rotation.from_matrix(rest[shoulder][:3, :3]),
            arm: upper_swing * Rotation.from_matrix(rest[arm][:3, :3]),
            elbow: lower_swing * Rotation.from_matrix(rest[elbow][:3, :3]),
            wrist: lower_swing * Rotation.from_matrix(rest[wrist][:3, :3]),
        }
        for i, rotation in rotations.items():
            parent = parents[i]
            parent_rotation = rotations.get(
                parent, Rotation.from_matrix(rest[parent][:3, :3])
            )
            neutral[i] = parent_rotation.inv() * rotation
    return neutral


def target_rig(path):
    nodes = json.loads(path.read_text(encoding="utf8"))["nodes"]
    parents = {child: i for i, n in enumerate(nodes) for child in n.get("children", [])}
    # Use hierarchy order even when the glTF node array is not topologically sorted.
    order = []

    def visit(i):
        order.append(i)
        for child in nodes[i].get("children", []):
            visit(child)

    for i in range(len(nodes)):
        if i not in parents:
            visit(i)
    world, local = {}, {}
    for i in order:
        n = nodes[i]
        if "matrix" in n:
            m = np.array(n["matrix"]).reshape(4, 4).T
        else:
            m = np.eye(4)
            m[:3, :3] = Rotation.from_quat(
                n.get("rotation", [0, 0, 0, 1])
            ).as_matrix() @ np.diag(n.get("scale", [1, 1, 1]))
            m[:3, 3] = n.get("translation", [0, 0, 0])
        local[i] = m
        world[i] = world[parents[i]] @ m if i in parents else m
    names = {n["name"].rsplit("_0", 1)[0]: i for i, n in enumerate(nodes)}
    ids = [i for i in order if i in {names[k] for k in PAIRS}]
    return nodes, parents, order, world, local, names, ids


def bake(data, rig, clip_name):
    nodes, parents, order, rest, local, names, ids = rig
    joints = data["joints"].tolist()
    source = {names[k]: joints.index(v) for k, v in PAIRS.items()}
    poses = data["posed_joints"]
    rotations = data["global_rot_mats"]
    neutral = data["rest"]
    n = len(poses)
    # A mapped child defines the bone axis. Hands use the middle finger, which
    # follows the animated wrist in the target's ordinary skin hierarchy.
    aims = {"Hips": "Spine", "Spine": "Chest", "Chest": "Neck", "Neck": "Head"}
    for side in ("Left", "Right"):
        for a, b in (
            ("shoulder", "arm"),
            ("arm", "elbow"),
            ("elbow", "wrist"),
            ("leg", "knee"),
            ("knee", "ankle"),
            ("ankle", "toe"),
        ):
            aims[f"{side} {a}"] = f"{side} {b}"
    across = poses[:, joints.index("RightLeg")] - poses[:, joints.index("LeftLeg")]
    yaw = gaussian_filter1d(
        np.unwrap(np.arctan2(across[:, 2], -across[:, 0])), 3, mode="nearest"
    )
    remove_yaw = Rotation.from_rotvec(np.column_stack((np.zeros(n), -yaw, np.zeros(n))))
    worlds, mapped = {}, {}
    needed = set(ids)
    for i in ids:
        while i in parents:
            i = parents[i]
            needed.add(i)
    for i in order:
        if i not in needed:
            continue
        parent = parents.get(i)
        parent_rot = worlds.get(parent, Rotation.identity())
        if i not in source:
            worlds[i] = parent_rot * Rotation.from_matrix(local[i][:3, :3])
            continue
        key = nodes[i]["name"].rsplit("_0", 1)[0]
        correction = Rotation.identity()
        if key in aims:
            k = names[aims[key]]
            correction = align(
                rest[k][:3, 3] - rest[i][:3, 3], neutral[source[k]] - neutral[source[i]]
            )
        elif key.endswith("wrist"):
            side = key.split()[0]
            k = names["MiddleFinger1_" + side[0]]
            correction = align(
                rest[k][:3, 3] - rest[i][:3, 3],
                neutral[joints.index(side + "HandMiddle1")] - neutral[source[i]],
            )
        worlds[i] = (
            remove_yaw
            * Rotation.from_matrix(rotations[:, source[i]])
            * correction
            * Rotation.from_matrix(rest[i][:3, :3])
        )
        mapped[i] = smooth_rotation(parent_rot.inv() * worlds[i], 0.75)

    # The unprompted clip means often put both hands in front of the chest.
    # Keep the rhythmic motion around each mean and place it around a relaxed
    # reference. Use a distinct bent-elbow pose for the quicker clips.
    running = clip_name.startswith(("run_", "jog-"))
    arms = relaxed_arms(rest, parents, names, running)
    for i, neutral_pose in arms.items():
        residual = (mapped[i].mean().inv() * mapped[i]).as_rotvec()
        # Reduce large changes at the wrist and shoulder, which carry less
        # locomotion signal than the upper arm and elbow.
        joint = nodes[i]["name"].split("_0", 1)[0]
        gain = 0.35 if joint.endswith(("wrist", "shoulder")) else 0.8
        mapped[i] = neutral_pose * Rotation.from_rotvec(gain * residual)

    # Re-evaluate FK so matching features describe the edited poses.
    positions = {}
    for i in order:
        if i not in needed:
            continue
        parent = parents.get(i)
        parent_rot = worlds.get(parent, Rotation.identity())
        positions[i] = positions.get(parent, np.zeros(3)) + parent_rot.apply(
            local[i][:3, 3]
        )
        worlds[i] = parent_rot * mapped.get(i, Rotation.from_matrix(local[i][:3, :3]))
    quats = [mapped[i].as_quat() for i in ids]
    hips = names["Hips"]
    ankles = [names["Left ankle"], names["Right ankle"]]
    scale = (rest[hips][1, 3] - rest[ankles[0]][1, 3]) / (
        neutral[source[hips], 1] - neutral[source[ankles[0]], 1]
    )
    toes = [names["Left toe"], names["Right toe"]]
    dst_feet = np.stack([positions[i] for i in ankles], axis=1)
    # Generated poses use a world floor at y=0. The SOMA rest pose is
    # pelvis-relative and has toes below its origin.
    src_height = poses[:, [source[i] for i in toes], 1]
    dst_toes = np.column_stack([positions[i][:, 1] for i in toes])
    bind_toes = np.array([rest[i][1, 3] for i in toes])
    desired_height = np.maximum(src_height - 0.025, 0) * scale + bind_toes
    support = np.exp(
        -np.clip((src_height - src_height.min(axis=1, keepdims=True)) / 0.025, 0, 40)
    )
    support /= support.sum(axis=1, keepdims=True)
    lift = ((desired_height - dst_toes) * support).sum(axis=1)
    lift = gaussian_filter1d(
        median_filter(lift, size=3, mode="nearest"), 1.2, mode="nearest"
    )
    # A world vertical displacement expressed in the pelvis parent's local axes.
    hip_offset = (
        Rotation.from_matrix(rest[parents[hips]][:3, :3])
        .inv()
        .apply(np.outer(lift, [0, 1, 0]))
    )
    root = (
        gaussian_filter1d(
            poses[:, joints.index("Hips")][:, [0, 2]], 3, axis=0, mode="nearest"
        )
        * scale
    )
    feet = dst_feet - rest[hips][:3, 3]
    feet[:, :, 1] += lift[:, None]
    vel = np.gradient(feet, 1 / 30, axis=0)
    # Rotate future root displacement and facing into the current facing frame.
    features = []
    for delta in (6, 14, 23):
        j = np.minimum(np.arange(n) + delta, n - 1)
        diff = root[j] - root
        co, si = np.cos(yaw), np.sin(yaw)
        features.extend(
            [
                co * diff[:, 0] - si * diff[:, 1],
                si * diff[:, 0] + co * diff[:, 1],
                np.sin(yaw[j] - yaw),
                np.cos(yaw[j] - yaw),
            ]
        )
    result = np.column_stack(
        [*features, feet.reshape(n, 6), vel.reshape(n, 6), yaw, hip_offset, *quats]
    )
    if not np.isfinite(result).all():
        raise ValueError("non-finite retarget")
    return result.astype("<f4")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("clips", type=Path)
    parser.add_argument("gltf", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    rig = target_rig(args.gltf)
    args.output.mkdir(parents=True, exist_ok=True)
    meta = {
        "version": 1,
        "fps": 30,
        "model": "nvidia/Kimodo-SOMA-RP-v1.2",
        "revision": "0e3c89024d896064b6851201373f7834784a386b",
        "bones": [rig[0][i]["name"] for i in rig[-1]],
        "clips": [],
    }
    rows = []
    for p in sorted(args.clips.glob("*.npz")):
        with np.load(p) as data:
            clip = bake(data, rig, p.stem)
        meta["clips"].append({"name": p.stem, "frames": len(clip)})
        rows.append(clip)
    np.concatenate(rows).tofile(args.output / "poses.bin")
    write_index(rows, args.output)
    (args.output / "bank.json").write_text(
        json.dumps(meta, indent=2) + "\n", encoding="utf8"
    )


if __name__ == "__main__":
    main()
