# DNG: DefaultCrop и размеры кадра

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
