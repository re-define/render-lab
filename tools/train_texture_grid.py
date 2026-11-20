"""Train random-access 4x4 feature grids for the Modern Hall glTF base colors."""

import argparse
import io
import json
import math
import struct
from pathlib import Path

import numpy as np
import torch
from PIL import Image
from torch import nn
from torch.nn import functional as F


def base_colors(path):
    doc = json.loads(path.read_bytes())
    for material in doc["materials"]:
        binding = material["pbrMetallicRoughness"].get("baseColorTexture")
        if binding is None:
            continue
        image = doc["images"][doc["textures"][binding["index"]]["source"]]
        view = doc["bufferViews"][image["bufferView"]]
        with (path.parent / doc["buffers"][view["buffer"]]["uri"]).open("rb") as file:
            file.seek(view.get("byteOffset", 0))
            encoded = file.read(view["byteLength"])
        with Image.open(io.BytesIO(encoded)) as source:
            if source.mode != "RGB":
                raise ValueError(f"Expected opaque RGB base color: {image['name']}")
            yield material["name"], np.array(source), len(encoded)


def quantize(latent):
    return ((latent.clamp(-1, 1) + 1) * 127.5).round() / 127.5 - 1


@torch.no_grad()
def pack(width, height, grid, decoder):
    latent = ((grid.clamp(-1, 1) + 1) * 127.5).round().byte().cpu().numpy()
    weights = np.concatenate(
        [p.cpu().numpy().ravel() for p in decoder.parameters()]
    ).astype("<f2")
    return (
        struct.pack("<4sIII", b"FGM1", width, height, (width + 3) // 4)
        + weights.tobytes()
        + latent.tobytes()
    )


def decode(payload, device):
    magic, width, height, gw = struct.unpack_from("<4sIII", payload)
    if magic != b"FGM1" or gw != (width + 3) // 4:
        raise ValueError("Invalid feature grid header")
    params = np.frombuffer(payload, dtype="<f2", count=960, offset=16)
    w1, b1, w2, b2 = torch.tensor(params, device=device, dtype=torch.float32).split(
        (128, 16, 768, 48)
    )
    grid = np.frombuffer(payload, dtype=np.uint8, offset=1936).reshape(-1, 8)
    z = torch.tensor(grid, device=device, dtype=torch.float32) / 127.5 - 1
    hidden = F.relu(F.linear(z, w1.view(16, 8), b1))
    colors = F.linear(hidden, w2.view(48, 16), b2).clamp(0, 1)
    padded = colors.view((height + 3) // 4, gw, 4, 4, 3).permute(0, 2, 1, 3, 4)
    return padded.reshape(-1, gw * 4, 3)[:height, :width]


def train(rgb, steps, device):
    height, width, _ = rgb.shape
    padded = np.pad(rgb, ((0, -height % 4), (0, -width % 4), (0, 0)), mode="edge")
    patches = torch.from_numpy(padded).to(device=device, dtype=torch.float32) / 255
    patches = (
        patches.view((height + 3) // 4, 4, (width + 3) // 4, 4, 3)
        .permute(0, 2, 1, 3, 4)
        .reshape(-1, 48)
    )
    # PCA initializes the grid and a pair of ReLUs per axis. All parameters
    # subsequently train together, including independent nonlinear hidden units.
    center = patches.mean(0)
    residual = patches - center
    _, basis = torch.linalg.eigh(residual.T @ residual / len(patches))
    basis = basis[:, -8:].contiguous()
    scores = residual @ basis
    scale = (scores.std(0) * 3.5).clamp_min(1e-4)
    grid = nn.Parameter((scores / scale).clamp(-1, 1))
    decoder = nn.Sequential(nn.Linear(8, 16), nn.ReLU(), nn.Linear(16, 48)).to(device)
    with torch.no_grad():
        eye = torch.eye(8, device=device)
        decoder[0].weight.copy_(torch.cat((eye, -eye)))
        decoder[0].bias.zero_()
        decoder[2].weight.copy_(torch.cat((basis * scale, -basis * scale), dim=1))
        decoder[2].bias.copy_(center)
    target = torch.tensor(rgb, device=device, dtype=torch.float32) / 255

    def psnr(payload):
        return -10 * math.log10(F.mse_loss(decode(payload, device), target).item())

    initial = psnr(pack(width, height, grid, decoder))
    opt = torch.optim.Adam(
        [{"params": [grid], "lr": 0.008}, {"params": decoder.parameters(), "lr": 0.001}]
    )
    for step in range(steps):
        ids = torch.randint(len(grid), (2048,), device=device)
        values = grid[ids].clamp(-1, 1)
        features = values + (quantize(values) - values).detach()
        loss = F.mse_loss(decoder(features), patches[ids])
        opt.zero_grad(set_to_none=True)
        loss.backward()
        opt.step()
        if step == steps * 3 // 4:
            for group in opt.param_groups:
                group["lr"] *= 0.25
    payload = pack(width, height, grid, decoder)
    return payload, initial, psnr(payload)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scene", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--steps", type=int, default=1600)
    args = parser.parse_args()
    if args.steps < 1:
        parser.error("--steps must be positive")
    args.output.mkdir(parents=True, exist_ok=True)
    device = "cuda" if torch.cuda.is_available() else "cpu"
    hardware = torch.cuda.get_device_name() if device == "cuda" else "CPU"
    print(
        f"PyTorch {torch.__version__} · {hardware} · {args.steps} Adam steps per texture",
        flush=True,
    )
    torch.manual_seed(47)
    for name, rgb, source_bytes in base_colors(args.scene):
        if name not in ("Wood", "FloorTiles", "Wallpaper"):
            raise ValueError(f"Unknown Modern Hall material {name}")
        payload, initial, psnr = train(rgb, args.steps, device)
        (args.output / f"{name}.fgm").write_bytes(payload)
        print(
            f"{name}: {rgb.shape[1]}x{rgb.shape[0]}, embedded PNG {source_bytes} B, "
            f"RGB8 {rgb.nbytes} B, grid+MLP {len(payload)} B, RGB PSNR {initial:.2f} -> {psnr:.2f} dB",
            flush=True,
        )


if __name__ == "__main__":
    main()
