# DNG geometry: full ActiveArea

## Current policy (0.3.1)

Scrutivue displays the **full DNG ActiveArea**, ignoring the recommended
`DefaultCropOrigin` / `DefaultCropSize`. It does not resample or alter source
photographs. Sensor margins outside ActiveArea remain excluded; this is not an
option to expose optical-black pixels. Ordinary DNGs without explicit ActiveArea
retain LibRaw's camera-specific output geometry; JXL LinearRaw falls back to its
stored canvas.

The six requested Nikon Z8 NEFs and 36 corresponding DNGs store **8280x5520**
pixels. Each DNG has `ActiveArea=(0,0,5520,8280)` and recommends the same framing
as Nikon's NEF `CropArea`: `(x=12,y=8,width=8256,height=5504)`. With orientation 8,
both NEF and DNG now display **5520x8280**. The JPEG exports physically contain
only 8256x5504 pixels and remain **5504x8256** after EXIF orientation.

The prior 0.3.0 DNG output is the exact rectangle
`(x=8,y=12,width=5504,height=8256)` inside the new oriented full frame. Only the
previously hidden border is exposed; overlapping RGB16 values are unchanged.
Equal dimensions do not imply equal NEF/DNG processing or equal color samples.
Pair statistics still reject unequal dimensions rather than stretching JPEGs.

### Consistency and safeguards

- Metadata sizing, streamed tile coordinates, direct developed-tile completion
  and full LibRaw development all select the same ActiveArea and orientation.
- The pinned Redlamp LibRaw fork, development parameters and JPEG XL transport
  remain unchanged. Camera WB, color conversion and orientation still use LibRaw.
- DefaultCrop metadata remains in the in-memory full-image transport, but is
  validated rather than applied. Invalid coordinates, zero denominators and
  non-unit DefaultScale are not silently accepted. Scale is checked even without
  DefaultCrop; no hidden resizing is introduced.
- Mapping accounts for ActiveArea margins already removed by LibRaw. All eight
  orientations, including reflections, are supported without a second crop or
  rotation. Output that cannot contain the requested ActiveArea fails explicitly.
- Embedded previews may already be cropped or reduced; they are not authoritative
  full-resolution geometry or analysis data.

### Validation

Synthetic coverage includes SHORT/LONG/RATIONAL DefaultCrop, both TIFF
endiannesses, raw SubIFDs, nonzero ActiveArea margins, every orientation, corrupt
metadata and rescaled output. Header-only tests still work with damaged JXL pixel
data. Native integration compares the complete RGB16 frame with the same DNG
whose DefaultCrop tags are absent. Streamed completion is compared to fresh full
development, both with and without active margins.

The read-only actual-corpus regression compares all 36 newly exposed full frames
against the preserved prior application's cropped hashes and sums. Its optional
test is `full_active_area_preserves_every_previous_default_crop_sample`, taking
`SCRUTIVUE_DNG_GEOMETRY_BASELINE` and a fresh `SCRUTIVUE_DNG_GEOMETRY_OUTPUT`.
Release validation and artifact provenance are recorded under
`target/release-0.3.1/`; see [0.3.1 notes](release-0.3.1.md).

## Historical DefaultCrop implementation (0.3.0; superseded)

The notes below document the earlier intentional crop and its measurements.
Their cropped dimensions, hashes, timings and rules are historical, not the
0.3.1 display policy.

Полный DNG декодируется в RGB16 закреплённым Redlamp LibRaw
`f4956c404fba0c6035ce9e06353d4b4ffe068fc7`; замены форка нет. JPEG XL
предварительно распаковывается только в памяти. Исходники открываются read-only.

## Причина расхождения и измерение

The exact `AK8_0005_halo0.dng` source was checked; its private local directory is omitted from this public document:

| Метаданные | Значение |
| --- | --- |
| ImageWidth / ImageLength | 8280 / 5520 |
| ActiveArea, top/left/bottom/right | 0 / 0 / 5520 / 8280 |
| DefaultCropOrigin, x/y | 12 / 8 (LONG) |
| DefaultCropSize, width/height | 8256 / 5504 (LONG) |
| DefaultScale | 1/1, 1/1 |
| Orientation | 8; LibRaw flip=5, поворот 270° |
| JPEG того же имени | stored 8256×5504, EXIF 8 → **5504×8256** |

LibRaw сохраняет DefaultCrop в `sizes.raw_inset_crops`, но сам его не применяет:
`src/metadata/identify.cpp:1435` и opt-in `adjust_to_raw_inset_crop` в
`src/utils/utils_libraw.cpp`. Кроме того, его TIFF parser приводит RATIONAL к
`ushort`, теряя дробную часть. Поэтому crop читается из исходных TIFF-тегов,
а не из этого округлённого поля LibRaw.

До исправления полный RAW давал **5520×8280**, 274233600 байт RGB16.
Native-измерение подтвердило исходную область LibRaw `(left=0, top=0,
width=8280, height=5520)`, затем ориентацию 8. После исправления результат —
**5504×8256**, **272646144 байт RGB16**. Все samples побитно равны прямоугольнику
`(x=8,y=12,width=5504,height=8256)` предыдущего ориентированного результата.
Ресэмплинга, растяжения и перехода через RGB8 нет.

SHA-256 RGB16 (little-endian samples):

- До default crop: `494e8a77551dd71c244b92df45f3a67ccab96bd8812cb9dd10daec241ebb0574`.
- После: `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f`, сумма samples `1520657420819`.
- Независимо распакованный libjxl DNG из `target/dng-reference/uncompressed.dng`
  дал тот же размер, сумму и полный SHA-256 после crop.

SHA-256 исходного DNG до/после чтения:
`5158a92acd72f7e6f7427d6fc51652370785508cedc28666538a4b9d8d14cc07`.
JPEG: `0079375231ca781e7dc9a9f2de0d5da37493308e4f0c2cea4c51da2c48544b89`.

Дополнительно прочитаны заголовки четырёх `AK8_0005_halo{24,48,96,256}.dng` и
пяти соседних `AK8_{0036,0149,0988,1588,1714}_halo0.dng`: те же размеры/crop/
ориентация. Все 30 JPEG именно в `halo` имеют stored 8256×5504 и EXIF 8.
Пять соседних halo0 DNG также полностью декодированы в 5504×8256 с проверкой
неизменности исходного SHA-256. Другие каталоги корпуса не сканировались.

## Правила

- DefaultCropOrigin задан относительно верхнего левого угла ActiveArea, **до**
  ориентации. Из native-wrapper передаётся фактическая входная область LibRaw;
  уже удалённые active margins не вычитаются повторно.
- SHORT, LONG и RATIONAL поддерживаются. RATIONAL проверяются без float;
  origin и size округляются к ближайшему пикселю (0.5 вверх), как
  [Adobe DNG SDK `DefaultCropArea`](https://github.com/hfiguiere/dng_sdk/blob/main/dng_sdk/source/dng_negative.cpp).
  Если только округление вывело край на один пиксель наружу, прямоугольник
  сдвигается обратно. Невалидный crop за пределами ActiveArea не маскируется.
- Прямоугольник преобразуется во все восемь ориентаций LibRaw, включая зеркальные.
  Повторного поворота уже ориентированного полного RAW нет. Уже применённый
  декодером default crop не обрезается повторно.
- JPEG/raster loader уже применяет EXIF orientation через `ImageDecoder`.
  JPEG-экспорт, намеренно уменьшенный или дополнительно обрезанный редактором,
  остаётся своего размера: соседний JPEG **не** определяет размер DNG.
- Встроенные preview могут быть уменьшены и уже обрезаны; DefaultCrop полного
  RAW к ним повторно не применяется.

## Проверки и ограничения

`decode::dimensions(path: &Path) -> anyhow::Result<[u32; 2]>` предназначен для
фонового metadata worker: classic DNG использует ту же функцию проверки и
округления DefaultCrop, JPEG — размеры/EXIF из ImageDecoder, остальные RAW —
LibRaw `open_buffer` + `adjust_sizes_info_only`, **без** unpack/process.
Для точного пользовательского DNG header-вызов дал 5504×8256 за **0.175 мс**
(тёплый файловый кэш), без чтения JXL samples. Для JPEG — те же 5504×8256.
Для HE / HE* заголовки также совпали с полным decode: 8280×5520.

В `src/dng.rs` и `src/decode.rs` восемь тестов: lossless JXL/RGB16 transport,
оба endian, raw SubIFD, SHORT/LONG/RATIONAL crop, ненулевой ActiveArea, все
восемь ориентаций, уже обрезанная область, дробное округление на границе,
отказ для повреждённых тегов/масштабирования и JPEG EXIF без изменения файла.
Header-only тест намеренно повреждает JXL pixels и всё равно получает правильный
crop/orientation по метаданным. Интеграционный synthetic-тест проходит через настоящий pinned LibRaw для
integer/rational crop и поворотов 0/90/270° и сравнивает все RGB16 samples.

На 2026-10-31 **8/8 прошли** сначала изолированным rustc-harness из
`target/dng-crop-check`, затем в общем Cargo-прогоне: **73/73**, включая native
DX12. Release-сборка проверена на исходном и независимо распакованном DNG:
полные RGB16 SHA-256 совпадают. Корневой пользовательский EXE не изменён.

Контрольные Nikon HE / HE* NEF сохранили 8280×5520 и прежние SHA-256 RGB16:
`0823f42e07a4616fb0ec75f68baed88b5868c04e0661b42574ba0c1adc15c266` /
`e995586232443fe7ea8a46e6895b0315e6b695c19b8dda77c5fec924bd51f2e0`.

Не поддерживаются этим crop-путём: BigTIFF, нестандартные типы crop-тегов,
неединичный DefaultScale (нужен отдельный явно определённый ресэмплинг),
неоднозначный выбор raw IFD и crop вне фактического прямоугольника LibRaw.
Для этих геометрий не подбирается масштаб по JPEG; возвращается ошибка,
если они обнаружены в classic TIFF crop-пути. `DefaultUserCrop`, XMP-правки,
opcode/lens-warp геометрия и геометрия внешних экспортёров дополнительно здесь
не воспроизводятся. Для JPEG XL transport сохраняются ограничения
unsigned grey/RGB16 chunky tiles, classic TIFF и 1 GiB samples.
