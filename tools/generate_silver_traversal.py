"""Generate contact-course actions with sparse SOMA key poses, then retarget to Silver Wolf."""

import argparse
from pathlib import Path

import numpy as np
import torch
from bake_silver_wolf import align, bake, target_rig
from generate_kimodo import MODEL, EmptyPrompt
from kimodo import load_model
from kimodo.constraints import FullBodyConstraintSet
from scipy.spatial.transform import Rotation

# Each landmark specifies the frame at 30 Hz, travel fraction, sole
# height, thigh flexion, shin backswing, torso pitch and lateral roll in degrees.
ACTIONS = {
    "jump": (
        1.42,
        [
            (0, 0, 0, 8, 6, 4, 0),
            (7, 0.025, 0, 52, 45, 20, 0),
            (11, 0.09, 0.02, 12, 10, 8, 0),
            (18, 0.48, 0.48, 67, 67, 17, 0),
            (23, 0.76, 0.34, 44, 34, 12, 0),
            (28, 0.95, 0, 23, 14, 8, 0),
            (32, 1, 0, 53, 43, 22, 0),
            (39, 1, 0, 9, 8, 4, 0),
        ],
    ),
    "vault": (
        1.32,
        [
            (0, 0, 0, 10, 8, 6, 0),
            (7, 0.05, 0, 38, 33, 20, 16),
            (12, 0.19, 0.23, 48, 54, 17, 45),
            (18, 0.43, 0.88, 72, 60, 13, 91),
            (22, 0.63, 0.90, 67, 64, 10, 96),
            (27, 0.84, 0.40, 46, 37, 14, 47),
            (32, 0.96, 0, 39, 37, 17, 8),
            (39, 1, 0, 8, 6, 4, 0),
        ],
    ),
    "duck": (
        1.65,
        [
            (0, 0, 0, 12, 10, 7, 0),
            (10, 0.12, 0, 58, 52, 36, 0),
            (20, 0.28, 0, 72, 56, 45, 0),
            (31, 0.46, 0, 67, 68, 43, 0),
            (43, 0.66, 0, 71, 56, 44, 0),
            (51, 0.81, 0, 60, 56, 37, 0),
            (57, 0.93, 0, 38, 30, 24, 0),
            (64, 1, 0, 12, 10, 6, 0),
        ],
    ),
}


def key_pose(skeleton, kind, distance, key):
    frame, travel, height, thigh, shin, pitch, roll = key
    ref = skeleton.neutral_joints.cpu().numpy()
    names = skeleton.bone_order_names
    parents = skeleton.joint_parents.cpu().tolist()
    index = names.index
    tilt = Rotation.from_euler("zx", [roll, pitch], degrees=True)
    orientations = [tilt] * len(names)

    def aim(bone, child, direction):
        orientations[index(bone)] = align(
            ref[index(child)] - ref[index(bone)], direction
        )

    def segment(angle):
        rad = np.radians(angle)
        return [0, -np.cos(rad), np.sin(rad)]

    sideways = Rotation.from_euler("z", roll, degrees=True)
    for side, sign in [("Left", 1), ("Right", -1)]:
        stride = (
            sign * 15 * np.sin(frame * 0.23) if kind == "duck" else sign * 0.10 * thigh
        )
        aim(side + "Leg", side + "Shin", sideways.apply(segment(thigh + stride)))
        aim(side + "Shin", side + "Foot", sideways.apply(segment(-shin + stride)))
        orientations[index(side + "Foot")] = Rotation.from_euler(
            "z", roll * 0.35, degrees=True
        )
        if kind == "vault" and roll > 25:
            upper, lower = (
                ([-0.35, -1, 0.08], [-0.16, -1, -0.06])
                if side == "Right"
                else ([1, 0.3, 0.18], [0.9, 0.48, 0.38])
            )
        else:
            arm = (
                25
                if kind == "duck"
                else (92 if height > 0.10 else -23 if thigh > 35 else 8)
            )
            upper = np.array(segment(arm)) + [sign * 0.19, 0, 0]
            lower = np.array(segment(arm + 32)) + [sign * 0.12, 0, 0]
        aim(side + "Arm", side + "ForeArm", upper)
        aim(side + "ForeArm", side + "Hand", lower)
        orientations[index(side + "Hand")] = orientations[index(side + "ForeArm")]
    points = np.zeros_like(ref)
    for i, p in enumerate(parents):
        if p >= 0:
            points[i] = points[p] + orientations[p].apply(ref[i] - ref[p])
    floor = min(points[index(s + "ToeBase"), 1] for s in ("Left", "Right"))
    points += [0, height - floor + 0.025, distance * travel]
    return points, np.stack([r.as_matrix() for r in orientations])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument(
        "--gltf", type=Path, default=Path("assets/characters/silver-wolf/scene.gltf")
    )
    parser.add_argument("--only", choices=ACTIONS)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    rig = target_rig(args.gltf)
    model = load_model(MODEL, device="cuda", text_encoder=EmptyPrompt())
    skeleton = model.skeleton
    for seed, (name, (distance, keys)) in enumerate(ACTIONS.items(), 81):
        if args.only and args.only != name:
            continue
        pos, rot = zip(*(key_pose(skeleton, name, distance, k) for k in keys))
        cond = FullBodyConstraintSet(
            skeleton,
            torch.tensor([k[0] for k in keys], device="cuda"),
            torch.as_tensor(np.asarray(pos), dtype=torch.float32, device="cuda"),
            torch.as_tensor(np.asarray(rot), dtype=torch.float32, device="cuda"),
        )
        torch.manual_seed(seed)
        with torch.inference_mode():
            out = model(
                "",
                keys[-1][0] + 1,
                constraint_lst=[cond],
                num_denoising_steps=40,
                cfg_type="regular",
                cfg_weight=1.8,
                return_numpy=True,
                post_processing=False,
                progress_bar=lambda it, **kwargs: it,
            )
        out["joints"] = np.array(model.output_skeleton.bone_order_names)
        out["rest"] = model.output_skeleton.neutral_joints.cpu().numpy()
        rows = bake(out, rig, name, relax_arms=False)
        # Travel in metres, bind-local hip offset, then 21 bank-order quaternions.
        progress = np.interp(
            np.arange(len(rows)), [k[0] for k in keys], [k[1] for k in keys]
        )
        packed = np.column_stack([distance * progress, rows[:, 25:]]).astype("<f4")
        packed.tofile(args.output / (name + ".bin"))
        print(name, rows.shape[0], "frames", flush=True)


if __name__ == "__main__":
    main()
