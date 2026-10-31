"""Windows-only native smoke test. Requires Pillow and an unobscured desktop."""
import argparse
import atexit
import ctypes as c
from ctypes import wintypes as w
import json
from pathlib import Path
import subprocess
import time
from PIL import ImageGrab

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--path", default=str(ROOT / "target/fixtures/library-10000"))
parser.add_argument("--seconds", type=float, default=12)
parser.add_argument("--output", default=str(ROOT / "target/ui-smoke"))
parser.add_argument("--cpu", action="store_true")
parser.add_argument("--recursive", action="store_true")
parser.add_argument("--tail", action="store_true", help="Navigate to the final item of the virtual list")
parser.add_argument("--exe", default=str(ROOT / "target/debug/lumen-diff.exe"))
parser.add_argument("--readme-screenshot", type=Path)
args = parser.parse_args()
output = Path(args.output)
output.mkdir(parents=True, exist_ok=True)
user = c.WinDLL("user32", use_last_error=True)
user.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
user.SetWindowPos.restype = w.BOOL
user.WindowFromPoint.argtypes = [w.POINT]
user.WindowFromPoint.restype = w.HWND
user.GetAncestor.argtypes = [w.HWND, w.UINT]
user.GetAncestor.restype = w.HWND
dwm = c.WinDLL("dwmapi")
dwm.DwmGetWindowAttribute.argtypes = [w.HWND, w.DWORD, c.c_void_p, w.DWORD]
process = subprocess.Popen([args.exe, args.path]
    + (["--cpu"] if args.cpu else []) + (["--recursive"] if args.recursive else []))
def stop_test_process():
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=5)
atexit.register(stop_test_process)
user.SetProcessDPIAware()

def find_window():
    found = []
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    def callback(hwnd, _):
        pid = w.DWORD()
        user.GetWindowThreadProcessId(hwnd, c.byref(pid))
        if pid.value == process.pid and user.IsWindowVisible(hwnd):
            found.append(hwnd)
        return True
    callback = callback_type(callback)
    user.EnumWindows(callback, 0)
    return found[0] if found else None

hwnd = None
start = time.monotonic()
while time.monotonic() - start < 30:
    hwnd = find_window()
    if hwnd:
        break
    if process.poll() is not None:
        raise SystemExit("App exited before opening a window")
    time.sleep(0.1)
if not hwnd:
    process.terminate()
    raise SystemExit("No window after 30 seconds")
open_ms = (time.monotonic() - start) * 1000
user.SetForegroundWindow(hwnd)
user.ShowWindow(hwnd, 9)
times = []
timeouts = 0
start = time.monotonic()
while time.monotonic() - start < args.seconds:
    result = c.c_size_t()
    t = time.perf_counter()
    if not user.SendMessageTimeoutW(hwnd, 0, 0, 0, 2, 100, c.byref(result)):
        timeouts += 1
    times.append((time.perf_counter() - t) * 1000)
    time.sleep(0.05)

capture_fallbacks = 0
def capture_window(handle):
    global capture_fallbacks
    image = ImageGrab.grab(window=handle)
    if max(high for low, high in image.convert("RGB").getextrema()) <= 2:
        # PrintWindow may return black for GPUI/DX11 surfaces. Briefly raise
        # only this test window without stealing keyboard focus from the user.
        capture_fallbacks += 1
        try:
            for attempt in range(5):
                if not user.SetWindowPos(handle, -1, 0, 0, 0, 0, 0x0013):
                    raise c.WinError(c.get_last_error())
                time.sleep(0.25)
                rect = w.RECT()
                if not user.GetWindowRect(handle, c.byref(rect)):
                    raise OSError("Could not get screenshot window bounds")
                frame = w.RECT()
                if dwm.DwmGetWindowAttribute(handle, 9, c.byref(frame), c.sizeof(frame)) == 0:
                    rect = frame  # Exclude the invisible resize borders/shadow.
                center = w.POINT((rect.left + rect.right) // 2, (rect.top + rect.bottom) // 2)
                if user.GetAncestor(user.WindowFromPoint(center), 2) == handle:
                    image = ImageGrab.grab(bbox=(rect.left, rect.top, rect.right, rect.bottom), all_screens=True)
                    break
            else:
                raise RuntimeError("Test window is obscured; refusing to save another application's content")
        finally:
            user.SetWindowPos(handle, -2, 0, 0, 0, 0, 0x0013)
    if max(high for low, high in image.convert("RGB").getextrema()) <= 2:
        raise RuntimeError("Screenshot is black; a visible desktop is required")
    return image

def screenshot(name):
    image = capture_window(hwnd)
    image.save(output / (name + ".png"))
    return image

def click(x, y):
    position = coordinates(x, y)
    user.PostMessageW(hwnd, 0x200, 0, position)
    time.sleep(0.05)
    user.PostMessageW(hwnd, 0x201, 1, position)
    time.sleep(0.05)
    user.PostMessageW(hwnd, 0x202, 0, position)
    time.sleep(0.25)

def key(vk, down=True):
    user.PostMessageW(hwnd, 0x100 if down else 0x101, vk, 0)

scale = user.GetDpiForWindow(hwnd) / 96
def coordinates(x, y):
    return (round(y * scale) << 16) | round(x * scale)

client = w.RECT()
user.GetClientRect(hwnd, c.byref(client))
width, height = client.right / scale, client.bottom / scale
native_icon = bool(user.SendMessageW(hwnd, 0x7F, 1, 0))

time.sleep(0.3)
click(140, 270)
screenshot("selected-a")
click(140, 352)
time.sleep(4 if "raw" in args.path.lower() else 0.6)
image = screenshot("slider")
if args.readme_screenshot:
    args.readme_screenshot.parent.mkdir(parents=True, exist_ok=True)
    image.thumbnail((1600, 1200))
    image.save(args.readme_screenshot)

# Only clipping should change while these complete A/B textures are displayed.
viewer_width = width - 282 - 238
y = height * 0.55
user.PostMessageW(hwnd, 0x201, 1, coordinates(282 + viewer_width * 0.5, y))
drag_start = time.perf_counter()
for step in range(30):
    x = 282 + viewer_width * (0.5 + 0.25 * step / 29)
    user.PostMessageW(hwnd, 0x200, 1, coordinates(x, y))
    time.sleep(0.008)
user.PostMessageW(hwnd, 0x202, 0, coordinates(x, y))
drag_ms = (time.perf_counter() - drag_start) * 1000
time.sleep(0.15)
screenshot("slider-moved")
if args.tail:
    key(0x23); key(0x23, False)
    time.sleep(0.8)
    screenshot("library-tail")
for digit, mode in [(0x32, "diff"), (0x31, "single"), (0x34, "click-a")]:
    key(digit); key(digit, False)
    time.sleep(0.3)
    screenshot(mode)
key(0x20)
time.sleep(0.15)
screenshot("click-b")
key(0x20, False)
key(0x70); key(0x70, False)
time.sleep(0.4)
def help_window():
    found = []
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    def callback(candidate, _):
        pid = w.DWORD(); user.GetWindowThreadProcessId(candidate, c.byref(pid))
        title = c.create_unicode_buffer(256); user.GetWindowTextW(candidate, title, 256)
        if pid.value == process.pid and "справка" in title.value.lower(): found.append(candidate)
        return True
    callback = callback_type(callback)
    user.EnumWindows(callback, 0)
    return found[0] if found else None
help_hwnd = help_window()
if help_hwnd:
    capture_window(help_hwnd).save(output / "help.png")
user.SetForegroundWindow(hwnd)
click(140, 206)
time.sleep(0.8)
screenshot("cleared")
user.PostMessageW(hwnd, 0x10, 0, 0)
try:
    code = process.wait(timeout=10)
except subprocess.TimeoutExpired:
    process.terminate()
    raise SystemExit("Window close did not stop the application")
times.sort()
report = {"window_open_ms": open_ms,
    "responsive_probes": len(times), "timeouts_over_100ms": timeouts,
    "wm_null_median_ms": times[len(times)//2], "wm_null_max_ms": max(times),
    "exit_code": code, "screenshots": str(output), "native_icon": native_icon,
    "help_window_opened": bool(help_hwnd), "divider_drag_ms_30_moves": drag_ms,
    "screen_capture_fallbacks": capture_fallbacks,
    "dpi_scale": scale, "client_logical_size": [width, height],
    "note": "WM_NULL checks event-loop responsiveness, not end-to-end image latency."}
text = json.dumps(report, indent=2)
(output / "result.json").write_text(text + "\n", encoding="utf-8")
print(text)
