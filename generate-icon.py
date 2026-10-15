"""Rebuild the Windows icon from the original Compositor artwork (requires Pillow)."""

from io import BytesIO
from pathlib import Path
import struct

from PIL import Image


def main():
    root = Path(__file__).resolve().parent
    source_path = root / "assets" / "app-icon-1024.png"
    output = root / "assets" / "app-icon.ico"
    sizes = (16, 24, 32, 48, 64, 128, 256)
    with Image.open(source_path) as original:
        if original.size != (1024, 1024):
            raise ValueError("The original artwork must be 1024 by 1024 pixels.")
        source = original.convert("RGBA")
        frames = []
        for size in sizes:
            frame = source.resize((size, size), Image.Resampling.LANCZOS)
            encoded = BytesIO()
            frame.save(encoded, format="PNG", compress_level=9)
            frames.append(encoded.getvalue())

    header = struct.pack("<HHH", 0, 1, len(sizes))
    offset = len(header) + 16 * len(sizes)
    directory = bytearray()
    for size, frame in zip(sizes, frames):
        dimension = size if size < 256 else 0
        directory.extend(
            struct.pack("<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(frame), offset)
        )
        offset += len(frame)
    encoded_icon = header + directory + b"".join(frames)
    with Image.open(BytesIO(encoded_icon)) as icon:
        if icon.ico.sizes() != {(size, size) for size in sizes}:
            raise ValueError("Icon frame sizes failed verification.")
        for size in sizes:
            if icon.ico.getimage((size, size)).getchannel("A").getextrema() != (0, 255):
                raise ValueError(f"Icon frame {size} is missing transparency.")
    output.write_bytes(encoded_icon)
    print(f"{output}: {len(encoded_icon)} bytes; RGBA sizes {sizes}")


if __name__ == "__main__":
    main()
