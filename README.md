# Lumen Diff

Настольное приложение для сравнения фотографий и RAW. **Rust · GPUI / GPUI Kit · DX12 через wgpu.** Nikon HE/HE* читаются форком LibRaw из Redlamp.

![Интерфейс Lumen Diff](docs/images/ui.png)

- Добавление файлов и папок, включая подпапки; виртуальный список с фоновыми миниатюрами и изменением порядка.
- Первый выбранный щелчком файл — **A**, второй — **B**. Повторный щелчок снимает выбор; следующий новый файл заменяет B.
- **Single**, **Diff** с усилением 1/4/16/64/256×, **Both slider**, **Both click**.
- Зум у курсора, панорамирование, Fit / 100% / 200% / 400%; положение сохраняется между кадрами.
- Вычитание Gaussian с выбором радиуса для сравнения шума.
- Разделитель использует готовые текстуры и только меняет их отсечение — без повторного вычисления изображений.
- Кнопки добавления, рекурсивного обхода и очистки закреплены над списком; справка открывается отдельно (`F1`).

## Сборка и запуск

Нужны актуальный Rust, Python 3.12+ и C++17-компилятор; на Windows — Visual Studio Build Tools с Windows SDK.

```powershell
python scripts/prepare.py
cargo run --release
cargo run --release -- --recursive "D:\Photos"
cargo run --release -- "D:\Photos\A.NEF" "D:\Photos\B.NEF"
```

Готовый Windows binary после сборки: `target/release/lumen-diff.exe`. OpenCL не требуется. Иконка встроена в EXE.

Подробности, ограничения и команды проверок: [руководство](docs/manual.md), [замеры](docs/validation.md), [выбор DX12](docs/compute-decision.md). Первый decode RAW занимает секунды в фоне; измерения прогретого GPU-пути не означают гарантии input-to-screen ≤100 мс на любом компьютере.

MIT; LibRaw используется по CDDL-1.0. Уведомления: [LICENSES](LICENSES/README.md). На скриншоте — CC0 Nikon Z9 HE/HE* из raw.pixls.us, без внешнего UI-макета.
