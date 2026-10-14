"""在隔离 X11 会话中验证真实应用，不读取用户剪贴板或历史库。"""

import json
import os
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import tkinter as tk
import zlib
from pathlib import Path

if os.environ.get("KWIKPASTE_X11_TEST") != "1":
    raise SystemExit("请通过 scripts/linux/test-app.sh 在隔离 Xvfb 会话中运行")

binary = Path(sys.argv[1]).resolve()
output = Path(sys.argv[2] if len(sys.argv) > 2 else "target/linux-acceptance").resolve()
output.mkdir(parents=True, exist_ok=True)
rootdir = Path(tempfile.mkdtemp(prefix="run-", dir=output))
settings = {
    "onboarding": {"completed": True},
    "appearance": {"language": "en-US"},
    "general": {"autoStart": False},
    "clipboard": {"preview": {"spaceEnabled": True}},
}
data = rootdir / "data/com.fastthree.kwikpaste/prod"
(data / "config").mkdir(parents=True, exist_ok=True)
(data / "config/settings.json").write_text(json.dumps(settings))
env = os.environ | {
    "XDG_DATA_HOME": str(rootdir / "data"),
    "XDG_CONFIG_HOME": str(rootdir / "config"),
    "XDG_CACHE_HOME": str(rootdir / "cache"),
    "XDG_SESSION_TYPE": "x11",
}
env.pop("KWIKPASTE_SELFTEST", None)
env.pop("WAYLAND_DISPLAY", None)

window = tk.Tk(className="KwikPasteTestTarget")
window.title("KwikPaste isolated paste target")
window.geometry("640x200+30+30")
editor = tk.Text(window)
editor.pack(expand=True, fill="both")
editor.focus_force()
window.update()
log_path = data.parent / "logs/KwikPaste.log"
log_path.parent.mkdir(parents=True, exist_ok=True)
log_path.write_text("")
log = (rootdir / "stdout.log").open("w")
app = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT)
result = {}
owners = []


def pump(seconds=0.1):
    """让 Tk 响应 selection 请求和键盘事件。"""
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        window.update()
        time.sleep(0.01)


def wait(predicate, message, timeout=15):
    """在有限时间内等待行为；失败保留日志供定位。"""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return
        if app.poll() is not None:
            raise AssertionError(f"app exited {app.returncode}: " + log_path.read_text()[-3000:])
        pump()
    raise AssertionError(message + "\n" + log_path.read_text()[-1500:])


def rows():
    """只读本次验收创建的数据库。"""
    try:
        with sqlite3.connect(data / "db/clipboard.db") as conn:
            return conn.execute(
                "SELECT kind, content, platform FROM clipboard_items ORDER BY created_at DESC"
            ).fetchall()
    except sqlite3.Error:
        return []


def test_png():
    """生成确定性 RGB PNG，不依赖第三方图片库。"""
    def chunk(kind, payload):
        """写入 PNG 数据块的长度、类型、内容和 CRC。"""
        return (
            struct.pack(">I", len(payload)) + kind + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xffffffff)
        )

    pixels = (b"\0" + bytes([50, 130, 210]) * 40) * 30
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 40, 30, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(pixels))
        + chunk(b"IEND", b"")
    )


def key(value):
    """发送真实 XTest 按键，覆盖注册快捷键和普通窗口输入。"""
    subprocess.run(["xdotool", "key", "--clearmodifiers", value], check=True)


def visible_windows():
    """按窗口类定位，AppImage 启动器的 PID 不一定等于真实应用 PID。"""
    proc = subprocess.run(
        ["xdotool", "search", "--onlyvisible", "--class", "^KwikPaste$"],
        text=True, capture_output=True,
    )
    return proc.stdout.split()


def own(mime, content):
    """保持 xclip 在前台提供测试内容，退出时回收 selection owner。"""
    owner = subprocess.Popen(
        ["xclip", "-selection", "clipboard", "-t", mime, "-i", "-quiet"],
        stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    owners.append(owner)
    owner.stdin.write(content)
    owner.stdin.close()


try:
    wait(lambda: "panel hotkey registered" in log_path.read_text(), "hotkey registration missing")
    window.focus_force()
    editor.focus_set()
    pump()
    token = f"KwikPaste Linux integration {time.time_ns()}"
    window.clipboard_clear()
    window.clipboard_append(token)
    pump()
    wait(lambda: any(row[1] == token and row[2] == "linux" for row in rows()), "text capture failed")
    result["text_capture"] = True

    key("alt+c")
    wait(lambda: bool(visible_windows()), "panel did not open")
    pump(0.6)
    panels = visible_windows()
    assert panels, "no visible panel"
    subprocess.run(["import", "-window", panels[-1], str(rootdir / "panel.png")], check=True)
    key("Return")
    wait(lambda: token in editor.get("1.0", "end"), "paste did not reach original editor")
    result["paste_to_previous_window"] = True
    result["text_clipboard_roundtrip"] = True

    key("alt+c")
    pump(0.6)
    window.focus_force()
    editor.focus_set()
    pump()
    wait(lambda: not visible_windows(), "panel did not hide on blur")
    result["blur_hides_panel"] = True

    image_path = rootdir / "sample.png"
    image_path.write_bytes(test_png())
    own("image/png", image_path.read_bytes())
    wait(lambda: any(row[0] == "image" and row[2] == "linux" for row in rows()), "PNG capture failed")
    result["png_capture"] = True
    key("alt+c")
    pump(0.6)
    key("ctrl+c")
    pump(0.5)
    png = subprocess.run(
        ["xclip", "-selection", "clipboard", "-t", "image/png", "-o"],
        capture_output=True, timeout=4, check=True,
    ).stdout
    assert png == image_path.read_bytes(), "PNG write-back differs"
    result["png_clipboard_roundtrip"] = True

    # 按住空格预览图片；预览必须映射，但不能抢走主面板的输入焦点。
    panel_focus = subprocess.check_output(["xdotool", "getwindowfocus"]).strip()
    subprocess.run(["xdotool", "keydown", "space"], check=True)
    try:
        wait(lambda: len(visible_windows()) >= 2, "preview did not show")
        assert subprocess.check_output(["xdotool", "getwindowfocus"]).strip() == panel_focus, "preview stole focus"
        result["preview_without_focus_steal"] = True
    finally:
        subprocess.run(["xdotool", "keyup", "space"], check=True)
    wait(lambda: len(visible_windows()) == 1, "preview did not hide after key release")
    key("Escape")
    pump(0.2)

    file_path = rootdir / "sample file.txt"
    file_path.write_text("local test file")
    own("text/uri-list", (file_path.as_uri() + "\r\n").encode())
    wait(lambda: any(row[0] == "files" for row in rows()), "file capture failed")
    result["files_capture"] = True
    second = subprocess.run([str(binary), "--auto-launch"], env=env, capture_output=True, timeout=8)
    assert second.returncode == 0 and app.poll() is None, "single-instance handoff failed"
    result["single_instance"] = True
    key("alt+x")
    pump(1)
    assert visible_windows(), "preferences did not show"
    result["preferences_window"] = True
    result["rows_by_kind"] = {
        kind: sum(row[0] == kind for row in rows()) for kind in ["text", "image", "files"]
    }
finally:
    # 正常退出优先，失败时只结束本测试启动的进程。
    try:
        subprocess.run([str(binary), "--quit"], env=env, capture_output=True, timeout=6)
    except subprocess.TimeoutExpired:
        pass
    try:
        app.wait(timeout=6)
    except subprocess.TimeoutExpired:
        app.terminate()
        app.wait(timeout=4)
    window.clipboard_clear()
    window.update()
    window.destroy()
    for owner in owners:
        if owner.poll() is None:
            owner.terminate()
            owner.wait(timeout=3)
    log.close()
    (rootdir / "result.json").write_text(json.dumps(result, indent=2))

print(json.dumps(result, indent=2))
print(f"验收记录：{rootdir}")
