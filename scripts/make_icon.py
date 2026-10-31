"""Regenerate the original Lumen Diff PNG/ICO assets (requires Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

root = Path(__file__).resolve().parents[1] / "assets"
root.mkdir(exist_ok=True)
scale = 4
image = Image.new("RGBA", (512 * scale, 512 * scale))
draw = ImageDraw.Draw(image)
def rect(box, radius, fill):
    draw.rounded_rectangle(tuple(v * scale for v in box), radius * scale, fill=fill)
def polygon(points, fill):
    draw.polygon([(x * scale, y * scale) for x, y in points], fill=fill)
rect((8, 8, 504, 504), 108, "#232325")
rect((60, 100, 452, 412), 38, "#55555c")
rect((76, 116, 436, 396), 25, "#18181b")
rect((92, 132, 244, 380), 16, "#e66a56")
rect((268, 132, 420, 380), 16, "#aeb6cf")
polygon([(92, 328), (160, 242), (212, 305), (244, 274), (244, 380), (92, 380)], "#763f3a")
polygon([(268, 328), (336, 242), (388, 305), (420, 274), (420, 380), (268, 380)], "#535d78")
draw.ellipse((144*scale, 172*scale, 184*scale, 212*scale), fill="#f5d9cd")
draw.ellipse((320*scale, 172*scale, 360*scale, 212*scale), fill="#f5f0e9")
rect((250, 82, 262, 430), 6, "#f4e5dc")
rect((233, 224, 279, 288), 14, "#f4e5dc")
image = image.resize((512, 512), Image.Resampling.LANCZOS)
image.save(root / "lumen-diff.png")
image.save(root / "lumen-diff.ico", sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
