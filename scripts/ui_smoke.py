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
parser.add_argument("--path", default=str(ROOT / "target/fixtures/library-10000"), help="Fixture directory with at least two supported images")
parser.add_argument("--seconds", type=float, default=12)
parser.add_argument("--output", default=str(ROOT / "target/ui-smoke"))
parser.add_argument("--cpu", action="store_true")
parser.add_argument("--recursive", action="store_true")
parser.add_argument("--tail", action="store_true", help="Navigate to the final item of the virtual list")
parser.add_argument("--exe", default=str(ROOT / "target/debug/scrutivue.exe"))
parser.add_argument("--readme-screenshot", type=Path)
parser.add_argument("--statistics", action="store_true")
parser.add_argument("--picker", action="store_true")
parser.add_argument("--no-capture", action="store_true", help="Run native input/restart assertions when desktop capture is unavailable")
parser.add_argument("--streaming", action="store_true", help="Exercise paired mode/divider changes immediately after startup, before the normal warm-view checks")
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
settings_file = (output / f"settings-{time.time_ns()}.json").resolve()
settings_file.write_text(json.dumps({"version": 1, "mode": "Diff", "gain": 64, "sigma": 3.1, "metrics_edge": 512}), encoding="utf-8")
command = [args.exe, args.path, "--settings-file", str(settings_file),
           "--cache-dir", str((output / "thumbnails").resolve())]
command += (["--cpu"] if args.cpu else []) + (["--recursive"] if args.recursive else [])
process = subprocess.Popen(command)
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
def probe():
    global timeouts
    result = c.c_size_t()
    started = time.perf_counter()
    if not user.SendMessageTimeoutW(hwnd, 0, 0, 0, 2, 100, c.byref(result)):
        timeouts += 1
    times.append((time.perf_counter() - started) * 1000)
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
                covering=user.GetAncestor(user.WindowFromPoint(center), 2)
                title=c.create_unicode_buffer(256);user.GetWindowTextW(covering,title,256)
                pid=w.DWORD();user.GetWindowThreadProcessId(covering,c.byref(pid))
                raise RuntimeError(f"Test window is obscured; refusing to capture another app: test={handle}, covering={covering}/{title.value}/pid={pid.value}, test_pid={process.pid}, frame={(rect.left, rect.top, rect.right, rect.bottom)}, center={(center.x, center.y)}")
        finally:
            user.SetWindowPos(handle, -2, 0, 0, 0, 0, 0x0013)
    if max(high for low, high in image.convert("RGB").getextrema()) <= 2:
        raise RuntimeError("Screenshot is black; a visible desktop is required")
    return image

def screenshot(name):
    if args.no_capture:
        return None
    image = capture_window(hwnd)
    image.save(output / (name + ".png"))
    return image

class MouseInput(c.Structure):
    _fields_ = [("dx", w.LONG), ("dy", w.LONG), ("mouseData", w.DWORD),
                ("dwFlags", w.DWORD), ("time", w.DWORD), ("dwExtraInfo", c.c_size_t)]
class KeyboardInput(c.Structure):
    _fields_ = [("wVk", w.WORD), ("wScan", w.WORD), ("dwFlags", w.DWORD),
                ("time", w.DWORD), ("dwExtraInfo", c.c_size_t)]
class HardwareInput(c.Structure):
    _fields_ = [("uMsg", w.DWORD), ("wParamL", w.WORD), ("wParamH", w.WORD)]
class InputData(c.Union):
    _fields_ = [("mi", MouseInput), ("ki", KeyboardInput), ("hi", HardwareInput)]
class Input(c.Structure):
    _fields_ = [("type", w.DWORD), ("data", InputData)]
user.SendInput.argtypes = [w.UINT, c.POINTER(Input), c.c_int]
user.SendInput.restype = w.UINT
user.GetForegroundWindow.restype = w.HWND
kernel = c.WinDLL("kernel32", use_last_error=True)
keyboard_state = (c.c_ubyte * 256)()
attached_input_thread = None

def control_key(down):
    global attached_input_thread
    # GPUI reads GetKeyState, not MK_CONTROL on a posted mouse message.
    # Real Ctrl input must be targeted only at this test process's window.
    if down:
        user.SetForegroundWindow(hwnd)
        if user.GetForegroundWindow() != hwnd:
            # A locked/headless desktop can deny foreground activation. Share
            # keyboard state only between this script and our test UI thread;
            # never inject Ctrl into the foreground (unrelated) application.
            pid = w.DWORD()
            thread = user.GetWindowThreadProcessId(hwnd, c.byref(pid))
            assert pid.value == process.pid, "Refusing to attach to another application"
            assert user.AttachThreadInput(kernel.GetCurrentThreadId(), thread, True), c.WinError(c.get_last_error())
            attached_input_thread = thread
            assert user.GetKeyboardState(keyboard_state), c.WinError(c.get_last_error())
            state = (c.c_ubyte * 256).from_buffer_copy(keyboard_state)
            state[0x11] = state[0xA2] = 0x80
            assert user.SetKeyboardState(state), c.WinError(c.get_last_error())
    elif attached_input_thread is not None:
        assert user.SetKeyboardState(keyboard_state), c.WinError(c.get_last_error())
        assert user.AttachThreadInput(kernel.GetCurrentThreadId(), attached_input_thread, False), c.WinError(c.get_last_error())
        attached_input_thread = None
        return
    if attached_input_thread is not None:
        time.sleep(0.08)
        return
    event = Input(type=1, data=InputData(ki=KeyboardInput(wVk=0x11, dwFlags=0 if down else 2)))
    assert user.SendInput(1, c.byref(event), c.sizeof(event)) == 1, c.WinError(c.get_last_error())
    time.sleep(0.08)

def click(x, y, control=False):
    if control:
        control_key(True)
    try:
        position = coordinates(x, y)
        modifier = 8 if control else 0
        user.PostMessageW(hwnd, 0x200, modifier, position)
        time.sleep(0.05)
        user.PostMessageW(hwnd, 0x201, 1 | modifier, position)
        time.sleep(0.05)
        user.PostMessageW(hwnd, 0x202, modifier, position)
        time.sleep(0.25)
    finally:
        if control:
            control_key(False)

def key(vk, down=True):
    user.PostMessageW(hwnd, 0x100 if down else 0x101, vk, 0)

preference_waits = []
def prefs(until=None):
    # The 400 ms debounce is only a lower bound: background cache writes can
    # delay the atomic settings save, especially for a full-resolution RAW pair.
    # Await persisted state, never repeat input to make a failed gesture pass.
    started = time.monotonic()
    time.sleep(0.7)
    while True:
        saved = json.loads(settings_file.read_text(encoding="utf-8"))
        if until is None or until(saved):
            preference_waits.append((time.monotonic() - started) * 1000)
            return saved
        assert time.monotonic() - started < 15, ("Timed out waiting for persisted settings", saved)
        probe()
        time.sleep(0.05)

scale = user.GetDpiForWindow(hwnd) / 96
def coordinates(x, y):
    return (round(y * scale) << 16) | round(x * scale)

client = w.RECT()
user.GetClientRect(hwnd, c.byref(client))
width, height = client.right / scale, client.bottom / scale
native_icon = bool(user.SendMessageW(hwnd, 0x7F, 1, 0))
title = c.create_unicode_buffer(256)
user.GetWindowTextW(hwnd, title, len(title))
assert "Scrutivue" in title.value, title.value
first_row_y, second_row_y = 266, 348

early_streaming = None
if args.streaming:
    # Do this before the normal responsiveness wait: with large tiled DNGs,
    # selecting B now exercises a cold partner rather than only complete frames.
    early_started = time.monotonic()
    click(140, second_row_y, control=True)
    key(0x32); key(0x32, False)
    time.sleep(0.08)
    viewer_width = width - 282
    y = height * 0.55
    x = 282 + viewer_width * 0.5
    user.PostMessageW(hwnd, 0x200, 0, coordinates(x, y))
    user.PostMessageW(hwnd, 0x201, 1, coordinates(x, y))
    for step in range(40):
        x = 282 + viewer_width * (0.5 + 0.3 * step / 39)
        user.PostMessageW(hwnd, 0x200, 1, coordinates(x, y))
        time.sleep(0.008)
        probe()
    user.PostMessageW(hwnd, 0x202, 0, coordinates(x, y))
    moved = prefs(lambda p: p["mode"] == "Slider" and p["split"] > 0.75)
    assert 0.75 < moved["split"] < 0.85, moved
    key(0x33); key(0x33, False)
    prefs(lambda p: p["mode"] == "Diff")
    key(0x32); key(0x32, False)
    prefs(lambda p: p["mode"] == "Slider")
    early_streaming = {"interaction_ms": (time.monotonic() - early_started) * 1000,
        "divider_fraction": moved["split"], "slider_diff_slider": True,
        "note": "Early native input and persisted-state assertions; decoder readiness and visible pixels are not observed by this harness."}
    # Isolate the early scenario from the ordinary test's startup/selection
    # assumptions. Only this harness-owned settings file is reset, after exit.
    user.PostMessageW(hwnd, 0x10, 0, 0)
    assert process.wait(timeout=10) == 0, "Early paired input did not close cleanly"
    settings_file.write_text(json.dumps({"version": 1, "mode": "Diff", "gain": 64,
        "sigma": 3.1, "metrics_edge": 512}), encoding="utf-8")
    process = subprocess.Popen(command)
    hwnd = None
    started = time.monotonic()
    while time.monotonic() - started < 30:
        hwnd = find_window()
        if hwnd:
            break
        assert process.poll() is None, "App exited after early paired input"
        time.sleep(0.1)
    assert hwnd, "No window after the early paired input restart"
    user.SetForegroundWindow(hwnd)
    user.ShowWindow(hwnd, 9)

start = time.monotonic()
while time.monotonic() - start < args.seconds:
    probe()
    time.sleep(0.05)

time.sleep(0.3)
# Startup must remain effective Single with only A, despite a saved Diff choice.
time.sleep(4 if "raw" in args.path.lower() else 0.6)
assert prefs()["mode"] == "Diff", "Startup overwrote the preferred comparison mode"
screenshot("startup-single-a")
key(0x32); key(0x32, False)
assert prefs()["mode"] == "Diff", "Slider was available before choosing B"
click(140, second_row_y, control=True)
key(0x32); key(0x32, False)
assert prefs(lambda p: p["mode"] == "Slider")["mode"] == "Slider", "Ctrl-click did not add B"
# A plain click clears B; repeated clicks keep the sole A selected.
click(140, second_row_y)
key(0x33); key(0x33, False)
assert prefs()["mode"] == "Slider", "Plain click did not clear B or disabled Diff changed the preference"
click(140, second_row_y)
click(140, first_row_y, control=True)
key(0x33); key(0x33, False)
assert prefs(lambda p: p["mode"] == "Diff")["mode"] == "Diff", "Repeated plain click deselected A"
# Ctrl-click A must keep the pair. Ctrl-click B removes it temporarily.
click(140, second_row_y, control=True)
key(0x32); key(0x32, False)
assert prefs(lambda p: p["mode"] == "Slider")["mode"] == "Slider", "Ctrl-click A changed the reference"
click(140, first_row_y, control=True)
key(0x33); key(0x33, False)
assert prefs()["mode"] == "Slider", "Removing B lost the preferred mode or left Diff enabled"
screenshot("single-after-removing-b")
click(140, first_row_y, control=True)
screenshot("slider-restored-with-b")
# Explicit Single overrides the remembered comparison choice, even without B.
click(140, first_row_y, control=True)
key(0x31); key(0x31, False)
assert prefs(lambda p: p["mode"] == "Click")["mode"] == "Click"
click(140, first_row_y, control=True)
assert prefs()["mode"] == "Click"
# Restore natural A/B order for screenshots and the divider benchmark.
click(140, first_row_y)
click(140, second_row_y, control=True)
key(0x32); key(0x32, False)
assert prefs(lambda p: p["mode"] == "Slider")["mode"] == "Slider"
time.sleep(0.3)
image = screenshot("slider")
if args.readme_screenshot and image is not None:
    args.readme_screenshot.parent.mkdir(parents=True, exist_ok=True)
    image.thumbnail((1600, 1200))
    image.save(args.readme_screenshot)

# Open/dismiss the GPUI file context menu, without launching an external app.
position = coordinates(140, 266)
user.PostMessageW(hwnd, 0x200, 0, position)
user.PostMessageW(hwnd, 0x204, 2, position)
user.PostMessageW(hwnd, 0x205, 0, position)
time.sleep(0.2)
screenshot("file-context-menu")
key(0x1B); key(0x1B, False)
time.sleep(0.1)

# The normal warm-view divider check; --streaming also tests early input above.
viewer_width = width - 282
y = height * 0.55
initial_split = prefs()["split"]
user.PostMessageW(hwnd, 0x201, 1, coordinates(282 + viewer_width * initial_split, y))
drag_start = time.perf_counter()
for step in range(30):
    x = 282 + viewer_width * (initial_split + (0.75 - initial_split) * step / 29)
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
for digit, mode in [(0x33, "diff"), (0x31, "single-a")]:
    key(digit); key(digit, False)
    time.sleep(0.3)
    screenshot(mode)
key(0x20)
time.sleep(0.15)
screenshot("click-b")
key(0x20, False)
# Verify actual camera movement in Both click, not only delivery of messages.
key(0x38); key(0x38, False)
def drag(button, dx, dy, start=None):
    down, up, flag = (0x207, 0x208, 0x10) if button == "middle" else (0x201, 0x202, 1)
    x, y = start or (282 + viewer_width * 0.6, height * 0.55)
    user.PostMessageW(hwnd, 0x200, 0, coordinates(x, y))
    time.sleep(0.05)
    user.PostMessageW(hwnd, down, flag, coordinates(x, y))
    time.sleep(0.03)
    for step in range(1, 13):
        user.PostMessageW(hwnd, 0x200, flag, coordinates(x + dx * step/12, y + dy * step/12))
        time.sleep(0.012)
        if step % 3 == 0:
            probe()
    user.PostMessageW(hwnd, up, 0, coordinates(x + dx, y + dy))
before_pan = prefs(lambda p: p["mode"] == "Click" and p["camera"]["zoom"] == {"Fixed": 8.0})["camera"]["center"]
drag("middle", 48, 24)
after_middle = prefs(lambda p: p["camera"]["center"] != before_pan)["camera"]["center"]
assert after_middle != before_pan and all(a <= b for a,b in zip(after_middle,before_pan)), (before_pan, after_middle)
screenshot("click-middle-panned")
drag("left", -36, -18)
after_left = prefs(lambda p: p["camera"]["center"] != after_middle)["camera"]["center"]
assert after_left != after_middle and all(a >= b for a,b in zip(after_left,after_middle)), (after_middle, after_left)
user.PostMessageW(hwnd, 0x200, 0, coordinates(282 + viewer_width * 0.7, height * 0.65))
assert prefs()["camera"]["center"] == after_left, "Pan continued after release"
screenshot("click-left-panned")
# The navigator stays in the bottom-right, independently of the bottom-left HUD.
# Click outside the viewport box to recenter, then drag that box to pan.
map_left, map_top = width - 240, height - 150
map_y = map_top + 65
# At 200% DPI the small fixture nearly fits even at 800%; target the
# letterboxed image edges so the clicks are outside its large viewport box.
click(map_left + 1, map_y)
navigator_left = prefs(lambda p: p["camera"]["center"] != after_left)["camera"]["center"]
click(map_left + 210, map_y)
navigator_right = prefs(lambda p: p["camera"]["center"] != navigator_left)["camera"]["center"]
assert navigator_right[0] > navigator_left[0], (navigator_left, navigator_right)
click(map_left + 110, map_y)
# For a small image this click can be inside the viewport box; that starts a
# box drag without recentering, so a changed center is not required here.
navigator_before_drag = prefs()["camera"]["center"]
drag("left", -36, 0, start=(map_left + 110, map_y))
navigator_dragged = prefs(lambda p: p["camera"]["center"] != navigator_before_drag)["camera"]["center"]
assert navigator_dragged[0] < navigator_before_drag[0], (navigator_before_drag, navigator_dragged)
user.PostMessageW(hwnd, 0x200, 0, coordinates(map_left + 150, map_y))
assert prefs()["camera"]["center"] == navigator_dragged, "Navigator continued after release"
screenshot("navigator-panned")
# Repeated outward movement at an image edge must clamp, not lose the frame.
edge_drag = (viewer_width * 0.75, height * 0.45)
edge_start = (282 + viewer_width * 0.1, height * 0.2)
pan_state_barriers = 0
def settled_pan_preferences():
    global pan_state_barriers
    # A fixed delay can read the previous settings save and falsely conclude
    # that a large-image pan has reached its edge. Round-trip a camera-neutral
    # picker toggle after the posted mouse events; its persisted snapshot must
    # include the preceding camera updates. Restore the original picker state.
    enabled = prefs()["picker_enabled"]
    key(0x43); key(0x43, False)
    prefs(lambda p: p["picker_enabled"] != enabled)
    key(0x43); key(0x43, False)
    pan_state_barriers += 1
    return prefs(lambda p: p["picker_enabled"] == enabled)

previous_edge = None
edge_history = []
for _ in range(32):
    # Large RAWs at 800% need more than 32 gestures to reach the edge.
    # Stop only after a settled batch no longer changes the actual camera.
    for _ in range(8):
        drag("middle", *edge_drag, start=edge_start)
    edge = settled_pan_preferences()["camera"]["center"]
    edge_history.append(edge)
    if edge == previous_edge:
        break
    previous_edge = edge
else:
    raise AssertionError("Camera did not settle at an image edge within 256 drags")
drag("middle", *edge_drag, start=edge_start)
confirmed_edge = settled_pan_preferences()["camera"]["center"]
assert confirmed_edge == edge, ("Camera escaped the nearest legal edge", edge, confirmed_edge, edge_history)
assert all(0 <= v <= 1 for v in edge), edge
key(0x46); key(0x46, False)
# A burst of real WM_MOUSEWHEEL events exercises deferred zoom in/out.
def wheel_burst(delta, start=None, count=8):
    x, y = start or (282 + viewer_width * 0.5, height * 0.55)
    user.PostMessageW(hwnd, 0x200, 0, coordinates(x, y))
    time.sleep(0.05)
    screen = w.POINT(round(x * scale), round(y * scale))
    user.ClientToScreen(hwnd, c.byref(screen))
    position = ((screen.y & 0xffff) << 16) | (screen.x & 0xffff)
    for _ in range(count):
        user.PostMessageW(hwnd, 0x20A, (delta & 0xffff) << 16, position)
        time.sleep(0.003)
    probe()
prefs()
wheel_burst(120)
zoom_in = prefs(lambda p: "Fixed" in p["camera"]["zoom"])["camera"]["zoom"]
assert "Fixed" in zoom_in, zoom_in
wheel_burst(-120)
zoom_out = prefs(lambda p: p["camera"]["zoom"] != zoom_in)["camera"]["zoom"]
assert zoom_out["Fixed"] < zoom_in["Fixed"], (zoom_in, zoom_out)
# The Navigator consumes its own wheel events and applies the same factor once.
key(0x38); key(0x38, False)
prefs(lambda p: p["camera"]["zoom"] == {"Fixed": 8.0})
wheel_burst(-120, count=2)
reference_wheel_zoom = prefs(lambda p: p["camera"]["zoom"] != {"Fixed": 8.0})["camera"]["zoom"]
key(0x38); key(0x38, False)
before_map_wheel = prefs(lambda p: p["camera"]["zoom"] == {"Fixed": 8.0})["camera"]
wheel_burst(-120, start=(map_left + 120, map_y), count=2)
after_map_wheel = prefs(lambda p: p["camera"]["zoom"] != before_map_wheel["zoom"])["camera"]
# Compare against the main-view factor, including system wheel-line settings
# and zoom limits, without modifying the user's Windows preferences.
assert abs(after_map_wheel["zoom"]["Fixed"] - reference_wheel_zoom["Fixed"]) < 0.001, (reference_wheel_zoom, after_map_wheel)
wheel_burst(120, start=(map_left + 120, map_y), count=2)
map_zoom_restored = prefs(lambda p: p["camera"]["zoom"] != after_map_wheel["zoom"])["camera"]
assert abs(map_zoom_restored["zoom"]["Fixed"] - 8) < 0.001, map_zoom_restored
screenshot("navigator-wheel-zoom")
key(0x46); key(0x46, False)
# Native fullscreen on/off; the normal window dimensions must be restored.
before_fullscreen = w.RECT(); user.GetWindowRect(hwnd, c.byref(before_fullscreen))
key(0x7A); key(0x7A, False)
time.sleep(0.5)
fullscreen_style = user.GetWindowLongW(hwnd, -16)
assert not fullscreen_style & 0x00C00000, "F11 did not remove the titlebar"
screenshot("fullscreen")
key(0x7A); key(0x7A, False)
time.sleep(0.5)
assert user.GetWindowLongW(hwnd, -16) & 0x00C00000, "F11 did not restore windowed mode"
# Probe the event loop during repeated selection, not only before interaction.
for i in range(40):
    key(0x24 if i % 2 else 0x23); key(0x24 if i % 2 else 0x23, False)
    probe()
    time.sleep(0.03)
key(0x4C); key(0x4C, False)
time.sleep(0.5)
screenshot("library-collapsed")
key(0x4C); key(0x4C, False)
time.sleep(0.5)
if args.statistics:
    key(0x53); key(0x53, False)
    started = time.monotonic()
    while time.monotonic() - started < 15:
        result = c.c_size_t()
        t = time.perf_counter()
        if not user.SendMessageTimeoutW(hwnd, 0, 0, 0, 2, 100, c.byref(result)):
            timeouts += 1
        times.append((time.perf_counter() - t) * 1000)
        time.sleep(0.05)
    screenshot("statistics")
    key(0x53); key(0x53, False)
if args.picker:
    key(0x33); key(0x33, False)
    time.sleep(0.6)
    key(0x43); key(0x43, False)
    user.PostMessageW(hwnd, 0x200, 0, coordinates(282 + viewer_width * 0.45, height * 0.45))
    time.sleep(0.3)
    screenshot("picker-diff")
    key(0x38); key(0x38, False)
    time.sleep(0.5)
    user.PostMessageW(hwnd, 0x200, 0, coordinates(282 + viewer_width * 0.46, height * 0.45))
    time.sleep(0.2)
    screenshot("zoom-800-picker-1600")
    wheel_burst(-120, start=(282 + viewer_width * 0.46, height * 0.45), count=2)
    time.sleep(0.15)
    screenshot("picker-visible-after-wheel-without-mousemove")
    key(0x43); key(0x43, False)
    key(0x46); key(0x46, False)
key(0x70); key(0x70, False)
time.sleep(0.4)
def help_window():
    found = []
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)
    def callback(candidate, _):
        pid = w.DWORD(); user.GetWindowThreadProcessId(candidate, c.byref(pid))
        title = c.create_unicode_buffer(256); user.GetWindowTextW(candidate, title, 256)
        if pid.value == process.pid and "help" in title.value.lower(): found.append(candidate)
        return True
    callback = callback_type(callback)
    user.EnumWindows(callback, 0)
    return found[0] if found else None
help_hwnd = help_window()
assert help_hwnd, "F1 did not open the English Help window"
if help_hwnd and not args.no_capture:
    capture_window(help_hwnd).save(output / "help.png")
user.SetForegroundWindow(hwnd)
key(0x33); key(0x33, False)
key(0x38); key(0x38, False)
key(0x43); key(0x43, False)
key(0x4C); key(0x4C, False)
user.PostMessageW(hwnd, 0x10, 0, 0)
try:
    code = process.wait(timeout=10)
except subprocess.TimeoutExpired:
    process.terminate()
    raise SystemExit("Window close did not stop the application")
saved = json.loads(settings_file.read_text(encoding="utf-8"))
assert saved["mode"] == "Diff" and saved["picker_enabled"] and not saved["library_open"], saved
assert saved["camera"]["zoom"] == {"Fixed": 8.0}, saved
process = subprocess.Popen(command)
started = time.monotonic()
while time.monotonic() - started < 15:
    hwnd = find_window()
    if hwnd:
        break
    time.sleep(0.1)
assert hwnd, "Restart did not open a window"
reloaded = prefs()
for name in saved:
    if name != "window_size":
        assert saved[name] == reloaded[name], (name, saved[name], reloaded[name])
screenshot("settings-restored")
key(0x32); key(0x32, False)
assert prefs()["mode"] == "Diff", "Restart selected B or overwrote the remembered comparison mode"
screenshot("restart-single-without-b")
if args.statistics:
    key(0x53); key(0x53, False)
    time.sleep(2)
    screenshot("restart-single-a-noise")
user.PostMessageW(hwnd, 0x10, 0, 0)
assert process.wait(timeout=10) == 0
times.sort()
report = {"window_open_ms": open_ms,
    "responsive_probes": len(times), "timeouts_over_100ms": timeouts,
    "wm_null_median_ms": times[len(times)//2], "wm_null_max_ms": max(times),
    "exit_code": code, "screenshots": str(output), "native_icon": native_icon,
    "click_middle_pan": True, "click_left_pan": True, "pan_stops_on_release": True,
    "camera_edge_clamped": True, "fullscreen_roundtrip": True, "selection_stress": True,
    "file_context_menu_smoke": True,
    "wheel_zoom_in_out": True,
    "navigator_wheel_zoom_in_out_once": True,
    "navigator_click_recenter": True, "navigator_box_drag": True,
    "navigator_stops_on_release": True,
    "restart_preferences": True, "settings_file": str(settings_file),
    "startup_single_without_b": True, "plain_click_selects_only_a": True,
    "ctrl_click_adds_and_removes_b": True, "comparison_mode_requires_b": True,
    "unavailable_mode_preserves_preference": True,
    "screenshots_captured": not args.no_capture,
    "help_window_opened": bool(help_hwnd), "divider_drag_ms_30_moves": drag_ms,
    "screen_capture_fallbacks": capture_fallbacks,
    "early_paired_input": early_streaming,
    "pan_state_barriers": pan_state_barriers, "edge_pan_history": edge_history,
    "persisted_settings_max_wait_ms": max(preference_waits),
    "dpi_scale": scale, "client_logical_size": [width, height],
    "note": "WM_NULL checks event-loop responsiveness, not end-to-end image latency."}
text = json.dumps(report, indent=2)
(output / "result.json").write_text(text + "\n", encoding="utf-8")
print(text)
