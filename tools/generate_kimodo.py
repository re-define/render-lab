"""Generate a reproducible, trajectory-conditioned SOMA locomotion set on CUDA."""

import argparse
from pathlib import Path
from tempfile import TemporaryDirectory

import numpy as np
import torch
from kimodo import load_model
from kimodo.constraints import Root2DConstraintSet

MODEL = "Kimodo-SOMA-RP-v1.2"
# Forward speed at start/end, angular velocity, lateral speed (m/s and rad/s).
PATHS = {
    "idle": (0, 0, 0, 0),
    "slow-walk": (0.65, 0.65, 0, 0),
    "walk": (1.35, 1.35, 0, 0),
    "run": (3.0, 3.0, 0, 0),
    "walk-left": (1.35, 1.35, -1, 0),
    "walk-right": (1.35, 1.35, 1, 0),
    "jog-left": (2.4, 2.4, -0.7, 0),
    "jog-right": (2.4, 2.4, 0.7, 0),
    "start": (0, 2.3, 0, 0),
    "stop": (2.3, 0, 0, 0),
    "backward": (-0.9, -0.9, 0, 0),
    "strafe-left": (0, 0, 0, -1.2),
    "strafe-right": (0, 0, 0, 1.2),
}


class EmptyPrompt(torch.nn.Module):
    """Kimodo masks the entire text feature for an empty prompt."""

    def forward(self, texts):
        assert all(not text.strip() for text in texts)
        return torch.zeros(len(texts), 1, 4096), [0] * len(texts)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    model = load_model(MODEL, device="cuda", text_encoder=EmptyPrompt())
    skeleton = model.output_skeleton
    frames = 120
    for kind, (start, end, turn, side) in PATHS.items():
        paths = [args.output / f"{kind}_{seed}.npz" for seed in (41, 42, 43)]
        if all(p.exists() for p in paths):
            continue
        time = torch.arange(frames, device="cuda") / model.fps
        heading = turn * time
        forward = torch.stack((heading.sin(), heading.cos()), dim=-1)
        right = torch.stack((heading.cos(), -heading.sin()), dim=-1)
        velocity = (
            torch.linspace(start, end, frames, device="cuda")[:, None] * forward
            + side * right
        )
        path = velocity.cumsum(0) / model.fps
        constraint = Root2DConstraintSet(
            model.skeleton,
            torch.arange(frames, device="cuda"),
            path - path[:1],
            global_root_heading=torch.stack((heading.cos(), heading.sin()), dim=-1),
        )
        for seed, dest in zip((41, 42, 43), paths):
            if dest.exists():
                continue
            torch.manual_seed(seed)
            with torch.inference_mode():
                output = model(
                    "",
                    frames,
                    constraint_lst=[constraint],
                    num_denoising_steps=40,
                    cfg_type="regular",
                    cfg_weight=1.8,
                    return_numpy=True,
                    post_processing=False,
                    progress_bar=lambda it, **kw: it,
                )
            with TemporaryDirectory(dir=dest.parent) as staging:
                tmp = Path(staging) / dest.name
                np.savez_compressed(
                    tmp,
                    **{k: output[k] for k in ("global_rot_mats", "posed_joints")},
                    joints=skeleton.bone_order_names,
                    rest=skeleton.neutral_joints.cpu().numpy(),
                )
                tmp.replace(dest)
            print(dest.name, flush=True)


if __name__ == "__main__":
    main()
