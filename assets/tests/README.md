# Synthetic DNG transport fixture

`dng-jxl-tile.jxl` is a generated, lossless 16×16 RGB16 tile, not a photograph.
Flattened samples are `(i * 17431 + 50123) & 65535`.

Generated with official libjxl via imagecodecs 2026.3.6:

```
python scripts/test_dng.py --fixture
```

SHA-256: `ced3a83eeb6790da8155842f5c76dc52721b940bbde608cd7e908d5ef89e044e`.
Native tests construct TIFFs in both endiannesses with four copies of this tile,
exercise partial right/bottom tiles, raw SubIFDs and unchanged camera metadata.
All unpacked samples are checked against the integer formula, not against an
output produced by the decoder under test. Synthetic fixture: CC0.
