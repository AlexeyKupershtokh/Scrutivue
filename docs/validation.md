# Local validation

## Scrutivue selection, wheel zoom and statistics memory update

2026-11-01: `cargo test --locked -- --include-ignored --skip profile_dng` passed
**110 correctness tests**, including both native DX12 suites. Only the two opt-in
real-DNG profiling tests were excluded. `cargo fmt --all --check` and
`git diff --check` pass. Release builds succeeded as `target/release/scrutivue.exe`
with the matching `onnxruntime.dll`; Windows product/file metadata names Scrutivue.
Repository-root binaries were not replaced.

The final release RAW run, `target/ui-scrutivue-final-raw/result.json`, passed:
window **312 ms**, **681 WM_NULL probes**, **0 timeouts >100 ms**, maximum
**37.60 ms**, exit 0. It covers Single/one-A startup despite a saved comparison
preference, ordinary sole-A clicks, Ctrl B selection/toggling, unavailable modes,
remembered mode restoration, navigator wheel zoom exactly once in both directions,
main-view wheel zoom, pan/release/clamping, fullscreen, Statistics/Color picker,
English Help and preference restart. An earlier debug run also passed with CPU
fallback (199 probes, no timeouts, maximum 2.99 ms).

The final release **10,000 PNG** run,
`target/ui-scrutivue-final-10000/result.json`, passed the same input/restart
assertions plus list-tail navigation: window **313 ms**, **526 probes**,
**0 timeouts >100 ms**, maximum **6.04 ms**, exit 0.

The standalone CI smoke validates CLI startup, identity pixel statistics and the
embedded LPIPS using the adjacent runtime. The release also matches the saved
seeded PSNR/SSIM/MS-SSIM/GMSD/LPIPS references. Fresh Python/Torch reference checks
remain unavailable. The Windows workflow builds and packages the EXE, runtime,
build pins and dependency notices; `scripts/test_package.py` exercises the
extracted binary rather than relying on a DLL elsewhere in the workspace.
Local `scripts/package_windows.py` and `scripts/test_package.py` both passed,
including ZIP CRCs, runtime identity, dependency-notice coverage and actual LPIPS
inference from the extracted bundle. Known upstream notice-text omissions are
documented, not represented as recovered notices, in `LICENSES/Rust-supplemental/`.

Large-DNG statistics profiling is documented in [statistics-performance.md](statistics-performance.md).
For the requested decoded pair, cold complete reports measured **1.636 s at 512**
and **3.199 s at 1536**, including about **0.26 s of decoded-memory hashing**.
Live-frame completed hits skip hashing and all stages. These isolated optimized
developer-harness measurements exclude UI/rendering and full decode. Pair decode,
not isolated statistics, showed multi-GB temporary peaks. Streaming exact Triangle
resizing removes its large intermediate, ONNX releases activation-arena buffers,
and single-A/Statistics-open states suppress full-resolution neighbor prefetch.
Opening Statistics also cancels existing speculative jobs while preserving selected
loads; no numerical metric definitions or original RGB16 samples were changed.

Desktop capture is still blocked by **Backstop Window**. Native input assertions
are not a visual confirmation that the picker never flickers or that every layout
and neutral-grey contrast state looks correct. The code preserves the same-source
picker during main-wheel refinement; source/filter changes still invalidate it.
No fresh screenshot or input-to-photon latency is claimed.

The sections below describe earlier versions and measurements.

## Navigator, English UI and cache revision update

Current `cargo test --locked`: **97 passed, 0 failed**, with two native GPU tests and one opt-in DNG timing test excluded. The two native DX12 tests also pass separately with `cargo test --locked gpu_matches_cpu -- --ignored --nocapture`: **99 correctness tests passed in total**. Coverage includes the 1600% DPI-independent picker, navigator geometry/thumbnail cancellation and worker source validity/release, statistics reuse across new IDs and new decoded allocations, decode-cache revisions and cancellation, priority/coalescing, readable file sizes and folder path handling. `cargo fmt --all --check` and `git diff --check` pass.

`cargo build --release --locked` succeeded. Outputs stay under `target/release/` with the matching `onnxruntime.dll`; the repository-root EXE is not replaced.

Native release check on **10,000 PNGs**, `target/ui-smoke-current/result.json`: window **297 ms**, **503 WM_NULL probes**, **0 timeouts >100 ms**, maximum **7.34 ms**, exit 0. Verified Single-mode left/middle pan and release, camera limits, wheel zoom, fullscreen round trip, selection stress, context-menu opening, Statistics/Color picker, English Help, navigator click-to-recenter/box dragging/release, and preference restoration. DPI scale was 2.0. The input script now moves to a drag/scroll target before sending button events, and avoids routing its viewport stress gestures over the new navigator. External applications were not launched.

Final release rechecks, including RAW, also passed:

| Result under `target/ui-smoke/` | Window | WM_NULL probes | Timeouts >100 ms | Maximum probe |
| --- | ---: | ---: | ---: | ---: |
| `raw-release-persistence-fixed/result.json` | 297 ms | 604 | 0 | 8.55 ms |
| `raw-release-persistence-fixed-10s/result.json` | 313 ms | 690 | 0 | 9.32 ms |
| `10k-release-persistence-fixed/result.json` | 312 ms | 515 | 0 | 10.89 ms |

All three runs include navigator click/box drag/release, both main-view pan buttons, camera bounds, wheel zoom, fullscreen, repeated selection, Statistics/Color picker, English Help and preference restart checks; all exited 0. An earlier RAW run failed because the smoke script read an old settings snapshot after a fixed 700 ms delay. Temporary input tracing confirmed that native panning had occurred; the asynchronous save took longer. The script now awaits the expected persisted state, with a 15-second timeout and strict movement assertions, without repeating any gesture. No application input fix was needed. Temporary tracing was removed.

Capture is still blocked by the system **Backstop Window**; the capture test refused to save another application's window. Native interaction checks therefore used `--no-capture`. No fresh screenshot or visual-layout verification is claimed. WM_NULL measures event-loop responsiveness, not complete source-load or input-to-screen latency.

The release statistics CLI still matches the saved seeded reference pair: PSNR **31.06097378895171 dB**, LPIPS **0.002658627461642027**, no pending stages. A fresh Python/PIQ/PyTorch reference run was not performed because the former Torch development environment is unavailable. The application itself runs ONNX without Python.

Actual Services measurements on the requested halo0/halo24 DNG pair: the reproduced two-thread decode budget took **11.567 s** to render ready, versus **6.043 s** with the new shared six-thread budget. Clear/re-add returned both cached RGB16 frames in **0.581 ms**, excluding a fresh render. These are optimized debug-profile measurements, not release or complete GPUI presentation latency; the requested **2–3 s target remains unmet**. Full RGB16 and source hashes, stage timings, limitations and reproduction: [loading-performance.md](loading-performance.md).

The final release `--probe` also decoded the existing Nikon HE/HE* fixtures with unchanged full RGB16 hashes (`0823f42e…c15c266` and `e9955862…51f2e0`). The sequential per-file decode intervals were 3,011 ms and 2,767 ms; they are not the requested DNG-pair timing.

Application source has no remaining Cyrillic text. Historical notes below retain their original language and measurements; they are not new measurements of this update.

## Обновление взаимодействий и библиотеки

Текущий полный прогон `cargo test --locked -- --include-ignored`: **73 теста пройдено**, включая native DX12. Добавлены проверки ближайших границ камеры, восстановления невалидного центра, affine-превью текстуры с DPI, debounce 10 мс и последовательности поколений; исходной пиксельной лупы без сглаживания; LFU/продолжения частичной статистики/переиспользования ресайзов/оценки шума; обновления каталога, сохранения явно добавленных файлов, порядка и ревизий; DefaultCrop DNG и EXIF-ориентации JPEG.

Финальный native release-прогон `target/ui-interaction-final-raw/result.json`: окно **515 мс**, **558** WM_NULL probes, **0** таймаутов >100 мс, максимум **9.19 мс**. Проверены оба pan-жеста/отпускание, пределы камеры повторными жестами внутри viewport, реальные wheel-bursts zoom in/out, F11 в обе стороны, повторный выбор, открытие/закрытие контекстного меню, статистика/Color picker и восстановление настроек. Внешние приложения из меню намеренно не запускались. Без снимков: desktop capture по-прежнему не доступен. WM_NULL измеряет отзывчивость event loop, не задержку от мыши до показа изображения.

Финальная release-сборка на **10 000 PNG**, `target/ui-interaction-final-10000/result.json`: окно **297 мс**, **261** probes (включая жесты), **0** таймаутов, максимум **14.02 мс**, переход к концу списка и все перечисленные input/restart-проверки пройдены, exit 0. Видимое количество готовых миниатюр без desktop capture не оценивалось.

Точный пользовательский DNG после DefaultCrop: **5504×8256**, как ориентированный JPEG. Исходный JXL DNG и ранее независимо распакованный libjxl `target/dng-reference/uncompressed.dng` дали одинаковый полный RGB16 SHA-256: `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f`. Подробности: [dng.md](dng.md).

CLI сохранённой seeded-пары снова дал PSNR **31.06097378895171 dB**, LPIPS **0.002658627461642027** (прежние reference-значения). Новый запуск Python/PIQ/PyTorch cross-check не выполнялся: прежнее development-окружение с Torch отсутствует; native тесты и ONNX inference проходят без него.

Ниже сохранены замеры предыдущей функциональной сборки; они не являются повторными замерами новых взаимодействий.

31 октября 2026, Windows x86-64, Rust 1.99.1, NVIDIA GeForce RTX 3070, драйвер 32.0.15.8157. Прогоны выполнены локально, не по данным сторонних проектов. Профиль `dev` с оптимизацией уровня 2; C++ LibRaw собирается с оптимизацией 3.

## Корректность

`cargo test -- --include-ignored`: **35 тестов пройдено**, включая native DX12 regressions, которые обычно помечены `ignored`, чтобы обычные тесты работали и без GPU. Сравниваются все три режима, разные размеры, оба состояния A/B, Gaussian с σ 0.3/2.0/7.5 и packed RGB16 с нечётным числом samples. Допуск GPU/CPU — один уровень BGRA8. Отдельно проверяются порядок каналов для GPUI, усиление, край Gaussian, зум у курсора, стабильность идентификаторов при перестановке и coalescing render-заданий.

Новые проверки покрывают назначение/снятие A/B щелчками, рекурсивное и нерекурсивное добавление вложенных каталогов, подготовку полных A/B независимо от положения разделителя и геометрию clipping без изменения масштаба изображения.

Обновление floating UI / Color picker / anti-moiré добавляет проверки диапазона ½ Fit…800%, известных координат и sRGB→OKLCH для красного, размещения popup, плоских интерьеров пикселей с непрерывной границей, усреднения **после абсолютной разности**, отсутствия шахматного муара в синтетическом тесте. Дополнительный DX12 regression сравнивает CPU/GPU при масштабах 14/25/32/104/201/400/800%, с High pass и без. Это не доказательство отсутствия любого муара на любой фотографии.

## Метрики

`scripts/test_metrics.py` на seeded паре 256×192 сравнивает CLI со сторонними реализациями NumPy / PIQ и официальным LPIPS, с допуском 2e-6. PSNR: **31.060973789 dB**, SSIM Y: **0.9951588150**, MS-SSIM Y: **0.9961577862**, GMSD Y: **0.0103413934**. LPIPS-Squeeze: PyTorch **0.0026586246677**, Rust/ort **0.0026586274616**. На дополнительных 32×32, 65×37, 255×191 и 192×256 максимальное абсолютное отличие от PyTorch <9e-9. Это настоящий forward pass официальных весов, не приближение. Дополнительные native unit-тесты покрывают точность RGB16 (различие в 1 sample), идентичные кадры, маленькие изображения, несовпадение размеров и отмену.

LPIPS экспортирован в ONNX (opset 17) с официальными SqueezeNet/learned v0.1 весами; модель встроена в EXE. Исполнение — `ort 2.0.0-rc.12` и официальный CPU ONNX Runtime 1.24.4 с SHA-256 pin. Python и дочерних neural-процессов в приложении нет; загрузок из сети при сравнении нет. Native-тест проверяет идентичность, динамические размеры, `RunOptions::terminate` и успешный новый run после отменённого.

## JPEG XL DNG

На точном пользовательском `AK8_0005_halo0.dng` (DNG 1.7, compression 52546, RGB16, тайлы 256×256, Nikon Z8) старый LibRaw выдавал Unsupported file format. Новый путь распаковывает JPEG XL в памяти и передаёт виртуальный несжатый DNG тому же закреплённому Redlamp LibRaw.

Историческая проверка transport **до исправления DefaultCrop**: release `--probe` давал **5520×8280** после ориентации, **274233600 bytes RGB16**, decode **3163 мс**, миниатюру **75×112 / 8.8 мс**, сумму samples **1527256151262**. Независимый dev-скрипт `scripts/test_dng.py --source …` распаковал все тайлы официальным libjxl через imagecodecs 2026.3.6 в отдельный `target/dng-reference/uncompressed.dng`: SHA-256 RGB16 совпал — `494e8a77551dd71c244b92df45f3a67ccab96bd8812cb9dd10daec241ebb0574`. Исходный DNG не изменён.

Исправленный decode применяет объявленный `DefaultCropOrigin=(12,8)` / `DefaultCropSize=(8256,5504)`, который LibRaw лишь сохранял в метаданных. Новый результат **5504×8256**, **272646144 bytes RGB16** совпадает по геометрии с EXIF-ориентированным JPEG; каждый sample — точный crop прежнего полного кадра. SHA-256 `f8599b83cf742d5c9e83d957eae515e2069e99331f6ece3fb89f0d99c008131f` совпал с независимо распакованным DNG. **8/8** изолированных decode/DNG-тестов прошли; header-only `decode::dimensions` также возвращает final crop/ориентацию, без unpack. Release здесь не пересобирался. Подробные метаданные, корпус, SHA-256 и ограничения: [dng.md](dng.md).

Защита cache cleanup проверяется отдельно: обычные имена фотографий не считаются именами кэша; непустой каталог без маркера владельца не очищается даже при выборе его через `--cache-dir`.

## Пара 45.4 Мп

Команда:

```powershell
.\target\release\scrutivue.exe --benchmark --iterations 30
```

Пара синтетических RGB16 8256×5504, viewport 1920×1080. Зум меняется между 100/200/400%, центр сдвигается, усиление меняется между всеми пятью значениями. **Актуальная release-сборка с пирамидой разностей**, 30 итераций на сценарий:

| Операция | Время |
| --- | ---: |
| Первый upload двух кадров + Diff pyramid + render | 177.3 мс |
| Прогретый diff, median / p95 | 3.55 / 4.32 мс |
| Первая подготовка Gaussian и новой Diff pyramid, σ=2 | 116.8 мс |
| Прогретый high-pass diff, median / p95 | 3.50 / 4.23 мс |
| Смена σ на 1 / 2 / 4 / 8, включая новую Diff pyramid | 46.3 / 51.9 / 64.6 / 98.9 мс |

Числа включают GPU dispatch, ожидание GPU, readback и копию экранного буфера в host Vec. **Не включают GPUI texture upload, композицию, ожидание дисплея и полное время input-to-photon.** Первый upload, RAW decode, большая σ и изменение исходников не приравниваются к прогретому интерактивному пути. Цель 100 мс не является безусловной гарантией для каждого компьютера, 4K/8K viewport и всех радиусов.

## Nikon HE / HE*

`python scripts/fixtures.py raw` загружает два CC0 файла из manifest закреплённого форка, проверяя SHA-256. `--probe` проверяет и миниатюру, и полноценный decode/проявление:

| Nikon Z9 | Размер | Миниатюра | Decode + RGB16 |
| --- | --- | ---: | ---: |
| HE, `be91bac2…` | 8280×5520 | 2.4 мс | 2644 мс |
| HE*, `e113deb3…` | 8280×5520 | 1.1 мс | 2760 мс |

Суммы samples: 1,286,802,310,054 и 1,390,994,926,151 соответственно. Это проверки чтения конкретных файлов, не собственная сертификация всех камер и режимов форка. Первый decode RAW выполняется на CPU и занимает секунды, в фоне.

Повторный probe новой release-сборкой сохранил эти суммы: HE **2467 мс**, HE* **2499 мс**. SHA-256 RGB16: HE `0823f42e07a4616fb0ec75f68baed88b5868c04e0661b42574ba0c1adc15c266`, HE* `e995586232443fe7ea8a46e6895b0315e6b695c19b8dda77c5fec924bd51f2e0`.

## Нативный UI

Актуальный `target/ui-onnx-input-v1/result.json`: окно за **500 мс**, **377** WM_NULL probes, **0** таймаутов >100 мс, максимум **0.68 мс**, выход 0. Проверены панорамирование **средней и левой кнопками в Both click** по фактическому изменению сохранённого центра камеры, остановка после release, повторный запуск и восстановление всех полей настроек. Native icon и отдельная F1-справка найдены. Тест использует собственный `--settings-file` и не меняет пользовательский профиль.

Повторный прогон **актуального release** (`target/ui-release-onnx-v1`): окно **312 мс**, **378** probes, **0** таймаутов, максимум **0.70 мс**, обе кнопки pan / release / restart пройдены, выход 0. Отдельная проверка EXE без `onnxruntime.dll` подтвердила: native-метрики доступны, LPIPS возвращает понятную ошибку, а не обрушивает процесс.

Актуальный release на каталоге 10 000 PNG (`target/ui-release-10000-v2`, `--tail --no-capture`): окно **313 мс**, **80** probes за первые 4 с, **0** таймаутов, максимум **1.51 мс**. Переключение к концу списка, оба pan-жеста и восстановление настроек прошли, выход 0; визуальное количество готовых thumbnails в этом прогоне не оценивалось.

Захват актуального окна сейчас перекрыт системным `Backstop Window`: capture отказался сохранять чужое окно, поэтому этот прогон выполнен с `--no-capture`. Новый скриншот не выдаётся за проверенный; README пока содержит снимок базовой сборки. До изменения этих трёх режимов floating UI / Color picker / 800% визуально проверялись в `target/ui-floating-v1` (376 probes, 0 таймаутов, максимум 3.21 мс).

Ниже — **исторические прогоны базовой сборки**, до плавающих настроек/ONNX/удаления одиночного режима:

`scripts/ui_smoke.py` запускает собранное приложение, адресует только его собственное окно, проверяет отзывчивость `WM_NULL`, выбирает изображения, двигает разделитель, переключает режимы клавишами, удерживает/отпускает пробел в Both click, открывает F1-справку, очищает список и закрывает окно с проверкой завершения процесса. Проверяется наличие native window icon. Для GPU-окон, возвращающих чёрный PrintWindow, используется снимок экранной области временно поднятого тестового окна; нужен доступный рабочий стол.

- Каталог из **10 000 PNG**: окно появилось за 1047 мс; за 12 секунд — 235 проверок отзывчивости, **0 таймаутов >100 мс**, максимальный ответ 3.67 мс. На снимке после End видны строка №10 000, закреплённые кнопки и 1067/10000 готовых миниатюр. Очистка во время оставшейся фоновой работы оставляет пустой список и освобождает выбранные кадры. Это небольшие синтетические PNG, созданные hardlink-ами, а не benchmark чтения 10 000 RAW с диска.
- Папка с двумя реальными Z9 HE/HE*: окно за 516 мс; 60 проверок перед выбором кадров, **0 таймаутов >100 мс**, максимальный ответ 0.26 мс. После фонового decode на снимках подтверждены A/B у двух разных файлов, Diff с усилением рядом с кнопкой режима, Slider до/после движения границы, переключение на B пробелом, отдельная справка и пустой список после очистки. Нативная иконка найдена в обоих прогонах; процессы завершились с кодом 0.
- Движение разделителя проверено 30 событиями с интервалом около 8 мс. Время отправки этих событий — не измерение задержки до экрана. По коду и unit-тестам граница изменяет только clipping готовых текстур; Gaussian, decode и compute при её движении не запускаются.
- `cargo build --release --locked` успешно создаёт актуальный EXE. Release-прогон с `--recursive` на дереве из трёх PNG в корне и двух уровнях подпапок показал все три файла и включённый checkbox; окно открылось за 313 мс, выход 0. Для рекурсивности без UI также есть unit-тест с сопоставлением 1/3 файлов при выключенном/включённом обходе.
- Измерение `WM_NULL` проверяет отсутствие длительной блокировки event loop, но не доказывает время реакции каждого контрола или задержку кадра до экрана.

Воспроизведение:

```powershell
python scripts/fixtures.py library
cargo build
python scripts/ui_smoke.py --tail
python scripts/ui_smoke.py --path target/fixtures/raw --seconds 8 --output target/ui-raw-smoke
python scripts/ui_smoke.py --cpu --seconds 3 --output target/ui-cpu-smoke
python scripts/ui_smoke.py --exe target/release/scrutivue.exe --path target/fixtures/raw --seconds 5 --output target/ui-release-raw --readme-screenshot docs/images/ui.png
```

Последние JSON-выводы, тестовые RAW и снимки хранятся в `target/` и не входят в исходную поставку. Clippy в установленном toolchain отсутствует; `cargo clippy` поэтому не выполняет анализ. macOS/Linux, все модели камер, повреждённые RAW под sanitizer и точная end-to-end задержка 100 мс в этой среде не проверены.
