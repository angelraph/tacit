"""Generates the voice-over: one clip per scene in scenes.json, and
out/timing.json with each clip's length, which the recorder uses to make each
scene exactly as long as its narration.

    python voice.py            # every scene
    python voice.py bid ways   # just these
"""
import asyncio
import json
import re
import subprocess
import sys
from pathlib import Path

import edge_tts
import imageio_ffmpeg

HERE = Path(__file__).parent
OUT = HERE / "out" / "voice"
FFMPEG = imageio_ffmpeg.get_ffmpeg_exe()


def duration(path: Path) -> float:
    # ffmpeg prints "Duration: 00:00:07.34" for any input it can read.
    res = subprocess.run([FFMPEG, "-i", str(path)], capture_output=True, text=True)
    m = re.search(r"Duration: (\d+):(\d+):([\d.]+)", res.stderr)
    h, mnt, s = m.groups()
    return int(h) * 3600 + int(mnt) * 60 + float(s)


async def main(only):
    spec = json.loads((HERE / "scenes.json").read_text(encoding="utf-8"))
    OUT.mkdir(parents=True, exist_ok=True)
    timing_path = HERE / "out" / "timing.json"
    timing = json.loads(timing_path.read_text()) if timing_path.exists() else {}
    for scene in spec["scenes"]:
        if only and scene["id"] not in only:
            continue
        mp3 = OUT / f"{scene['id']}.mp3"
        await edge_tts.Communicate(scene["say"], spec["voice"], rate=spec.get("rate", "+0%")).save(str(mp3))
        timing[scene["id"]] = round(duration(mp3), 2)
        print(f"{scene['id']:>8}  {timing[scene['id']]:6.2f} s")
    timing_path.write_text(json.dumps(timing, indent=2))
    print(f"total {sum(timing.values()):.1f} s")


asyncio.run(main(set(sys.argv[1:])))
