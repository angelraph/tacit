"""Generates the voice-over: one clip per scene in scenes.json, and
out/timing.json with each clip's length, which the recorder uses to make each
scene exactly as long as its narration.

    python voice.py            # every scene
    python voice.py bid ways   # just these
    python voice.py --spec scenes-2min.json   # a shorter cut, kept apart

A spec with a "name" keeps its clips in out/voice-<name> and its lengths in
out/timing-<name>.json, so cuts never overwrite each other. Next to each clip,
<scene>.words.json holds when every word is spoken, which build.py turns into
captions.
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
FFMPEG = imageio_ffmpeg.get_ffmpeg_exe()


def duration(path: Path) -> float:
    # ffmpeg prints "Duration: 00:00:07.34" for any input it can read.
    res = subprocess.run([FFMPEG, "-i", str(path)], capture_output=True, text=True)
    m = re.search(r"Duration: (\d+):(\d+):([\d.]+)", res.stderr)
    h, mnt, s = m.groups()
    return int(h) * 3600 + int(mnt) * 60 + float(s)


def paths(spec):
    tag = f"-{spec['name']}" if spec.get("name") else ""
    return HERE / "out" / f"voice{tag}", HERE / "out" / f"timing{tag}.json"


async def main(spec_file, only):
    spec = json.loads((HERE / spec_file).read_text(encoding="utf-8"))
    OUT, timing_path = paths(spec)
    OUT.mkdir(parents=True, exist_ok=True)
    timing = json.loads(timing_path.read_text()) if timing_path.exists() else {}
    for scene in spec["scenes"]:
        if only and scene["id"] not in only:
            continue
        mp3 = OUT / f"{scene['id']}.mp3"
        tts = edge_tts.Communicate(scene["say"], spec["voice"], rate=spec.get("rate", "+0%"), boundary="WordBoundary")
        audio, words = bytearray(), []
        async for chunk in tts.stream():
            if chunk["type"] == "audio":
                audio += chunk["data"]
            elif chunk["type"] == "WordBoundary":
                # Offsets come in 100 ns ticks.
                start = chunk["offset"] / 1e7
                words.append([round(start, 3), round(start + chunk["duration"] / 1e7, 3), chunk["text"]])
        mp3.write_bytes(audio)
        (OUT / f"{scene['id']}.words.json").write_text(json.dumps(words), encoding="utf-8")
        timing[scene["id"]] = round(duration(mp3), 2)
        print(f"{scene['id']:>8}  {timing[scene['id']]:6.2f} s")
    timing_path.write_text(json.dumps(timing, indent=2))
    print(f"total {sum(timing.values()):.1f} s")


args = sys.argv[1:]
spec_file = "scenes.json"
if args[:1] == ["--spec"]:
    spec_file, args = args[1], args[2:]
asyncio.run(main(spec_file, set(args)))
