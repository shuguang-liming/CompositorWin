# Development-only independent runtime oracle; this is not an application dependency.
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "build/onnx-validation-libs"))
import numpy as np
import onnxruntime as ort
from PIL import Image
root = Path(__file__).resolve().parents[2]
image = Image.open(root / "build/segmentation-samples/original.jpg").convert("RGB")
rgb = np.array(image.resize((320, 320), Image.Resampling.LANCZOS)).astype(np.float32)
rgb /= max(np.max(rgb), 1e-6)
rgb = (rgb - np.array([0.485, 0.456, 0.406], np.float32)) / np.array([0.229, 0.224, 0.225], np.float32)
tensor = np.expand_dims(rgb.transpose(2, 0, 1), 0)
session = ort.InferenceSession(str(root / "assets/u2netp.onnx"), providers=["CPUExecutionProvider"])
prediction = session.run(None, {session.get_inputs()[0].name: tensor})[0][0, 0]
mask = (prediction - prediction.min()) / max(prediction.max() - prediction.min(), 1e-8)
mask = Image.fromarray((mask * 255).astype(np.uint8)).resize(image.size, Image.Resampling.BILINEAR)
mask.save(root / "build/segmentation-samples/reference-mask.png")
rust = np.array(Image.open(root / "build/segmentation-samples/u2netp/mask.png"))[:, :, 0].astype(np.int16)
reference = np.array(mask).astype(np.int16)
difference = np.abs(rust - reference)
print({"reference_foreground_percent": float(np.mean(reference >= 128) * 100), "mean_absolute_alpha_difference": float(difference.mean()), "max_alpha_difference": int(difference.max()), "alpha_within_16_percent": float(np.mean(difference <= 16) * 100)})

