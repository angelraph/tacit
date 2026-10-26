"""Assembles the demo video from out/raw/<scene>.webm and out/voice/<scene>.mp3.

For each scene: drop the stretches marked as cuts (loading, proving, waiting
for blocks), fade in and out, put the narration on top, and hold the scene
until the narration ends. Then join every scene and lay a quiet generated
music bed under the whole thing.

    python build.py            # out/tacit-demo.mp4
    python build.py --spec scenes-2min.json   # out/tacit-demo-2min.mp4

A named spec reads out/voice-<name> and out/timing-<name>.json (see voice.py)
and may set "max_speed": how much faster than life its footage may play.
"""
import json
import subprocess
import sys
from pathlib import Path

import imageio_ffmpeg

HERE = Path(__file__).parent
OUT = HERE / "out"
FF = imageio_ffmpeg.get_ffmpeg_exe()
FPS = 30
LEAD = 0.35  # narration starts this long after the scene does


def run(args):
    res = subprocess.run([FF, "-y", "-loglevel", "error", *args], capture_output=True, text=True)
    if res.returncode:
        raise SystemExit(res.stderr)


def keep_ranges(length, cuts):
    out, at = [], 0.0
    for a, b in sorted(cuts):
        if a > at + 0.05:
            out.append((at, a))
        at = max(at, b)
    if length > at + 0.05:
        out.append((at, length))
    return out


def scene_clip(sid, voice_len, voice_dir, clip_dir, max_speed):
    meta = json.loads((OUT / "raw" / f"{sid}.json").read_text())
    ranges = keep_ranges(meta["length"], meta["cuts"])
    kept = sum(b - a for a, b in ranges)
    # Footage longer than the narration plays a little faster (at most
    # max_speed) rather than losing its ending; whatever is left over is trimmed.
    target = voice_len + LEAD + 1.2
    speed = min(max_speed, kept / target) if kept > target + 1.3 else 1.0
    dur = max(voice_len + LEAD + 0.5, min(kept / speed, voice_len + LEAD + 2.5))
    parts = "".join(
        f"[0:v]trim=start={a:.3f}:end={b:.3f},setpts=PTS-STARTPTS[p{i}];" for i, (a, b) in enumerate(ranges)
    )
    joined = "".join(f"[p{i}]" for i in range(len(ranges)))
    # Hold the last frame if the narration outlasts the footage.
    vf = (
        f"{parts}{joined}concat=n={len(ranges)}:v=1:a=0,setpts=PTS/{speed:.3f},fps={FPS},"
        f"tpad=stop_mode=clone:stop_duration=10,trim=duration={dur:.3f},"
        f"fade=t=in:st=0:d=0.35,fade=t=out:st={dur - 0.35:.3f}:d=0.35,format=yuv420p[v];"
        f"[1:a]adelay={int(LEAD * 1000)}|{int(LEAD * 1000)},apad,atrim=duration={dur:.3f}[a]"
    )
    out = clip_dir / f"{sid}.mp4"
    out.parent.mkdir(exist_ok=True)
    run([
        "-i", str(OUT / "raw" / f"{sid}.webm"), "-i", str(voice_dir / f"{sid}.mp3"),
        "-filter_complex", vf, "-map", "[v]", "-map", "[a]",
        "-c:v", "libx264", "-preset", "medium", "-crf", "17", "-r", str(FPS),
        "-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2", str(out),
    ])
    return out, dur


def music(total, tag):
    """A soft, slowly breathing pad: three detuned sine chords, low passed."""
    out = OUT / f"music{tag}.wav"
    chord = "+".join(
        f"0.05*sin(2*PI*{f}*t)*(0.6+0.4*sin(2*PI*{lfo}*t))"
        for f, lfo in [(110, 0.05), (164.81, 0.07), (220.5, 0.04), (277.18, 0.06), (329.63, 0.03)]
    )
    run([
        "-f", "lavfi", "-i", f"aevalsrc='{chord}':s=48000:d={total:.2f}",
        "-af", f"lowpass=f=900,afade=t=in:d=3,afade=t=out:st={total - 4:.2f}:d=4,volume=0.6",
        "-ac", "2", str(out),
    ])
    return out


def main(spec_file):
    spec = json.loads((HERE / spec_file).read_text(encoding="utf-8"))
    tag = f"-{spec['name']}" if spec.get("name") else ""
    timing = json.loads((OUT / f"timing{tag}.json").read_text())
    voice_dir, clip_dir = OUT / f"voice{tag}", OUT / f"clips{tag}"
    max_speed = spec.get("max_speed", 1.7)
    clips, total = [], 0.0
    for scene in spec["scenes"]:
        sid = scene["id"]
        if not (OUT / "raw" / f"{sid}.webm").exists():
            print(f"skipping {sid}: not recorded yet")
            continue
        clip, dur = scene_clip(sid, timing[sid], voice_dir, clip_dir, max_speed)
        clips.append(clip)
        total += dur
        print(f"{sid:>8}  {dur:5.1f} s")
    listing = OUT / f"clips{tag}.txt"
    listing.write_text("".join(f"file '{c.as_posix()}'\n" for c in clips))
    joined = OUT / f"joined{tag}.mp4"
    run(["-f", "concat", "-safe", "0", "-i", str(listing), "-c", "copy", str(joined)])
    bed = music(total, tag)
    final = OUT / f"tacit-demo{tag}.mp4"
    run([
        "-i", str(joined), "-i", str(bed),
        "-filter_complex", "[1:a]volume=0.35[m];[0:a][m]amix=inputs=2:duration=first:dropout_transition=0[a]",
        "-map", "0:v", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart", str(final),
    ])
    print(f"{final} ({total:.1f} s)")


args = sys.argv[1:]
main(args[1] if args[:1] == ["--spec"] else "scenes.json")
