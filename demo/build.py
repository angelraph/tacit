"""Assembles the demo video from out/raw/<scene>.webm and out/voice/<scene>.mp3.

For each scene: drop the stretches marked as cuts (loading, proving, waiting
for blocks), fade in and out, put the narration on top, and hold the scene
until the narration ends. Then join every scene and lay a quiet generated
music bed under the whole thing.

    python build.py            # out/tacit-demo.mp4
    python build.py --spec scenes-2min.json   # out/tacit-demo-2min.mp4

A named spec reads out/voice-<name> and out/timing-<name>.json (see voice.py)
and may set "max_speed": how much faster than life its footage may play.
When voice.py left word timings for every scene, the build also writes
captions: out/tacit-demo<tag>.srt for uploading, and a copy of the video with
them burned in, out/tacit-demo<tag>-captions.mp4.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

import imageio_ffmpeg

HERE = Path(__file__).parent
OUT = HERE / "out"
FF = imageio_ffmpeg.get_ffmpeg_exe()
FPS = 30
LEAD = 0.35  # narration starts this long after the scene does
LINE = 44  # caption characters per line; two lines at most
# White on a soft dark box, bottom centre. Sizes are in libass's 384x288 frame,
# so 15 is about 56 px at 1080p: comfortable on a phone.
CAPTION_STYLE = (
    "FontName=Segoe UI Semibold,FontSize=15,PrimaryColour=&H00FFFFFF,BackColour=&H70000000,"
    "BorderStyle=3,Outline=1.2,Shadow=0,Alignment=2,MarginV=16"
)


def run(args):
    res = subprocess.run([FF, "-y", "-loglevel", "error", *args], capture_output=True, text=True)
    if res.returncode:
        raise SystemExit(res.stderr)


def duration(path):
    # ffmpeg prints "Duration: 00:00:07.34" for any input it can read.
    res = subprocess.run([FF, "-i", str(path)], capture_output=True, text=True)
    h, m, s = re.search(r"Duration: (\d+):(\d+):([\d.]+)", res.stderr).groups()
    return int(h) * 3600 + int(m) * 60 + float(s)


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


def cues(say, words):
    """Groups the spoken words into captions of up to two lines, breaking after
    sentences and, in long ones, after commas. The narration's own text is
    shown, so punctuation survives; the speech engine only supplies timing."""
    tokens = say.split()
    if len(tokens) != len(words):
        tokens = [w[2] for w in words]
    out, cur = [], []
    for i, (tok, (a, b, _)) in enumerate(zip(tokens, words)):
        if cur and len(" ".join([*(t for t, _, _ in cur), tok])) > 2 * LINE:
            out.append(cur)
            cur = []
        cur.append((tok, a, b))
        text = " ".join(t for t, _, _ in cur)
        if i + 1 < len(tokens) and (tok[-1] in ".?!" and len(text) > 18 or tok[-1] in ",:" and len(text) > LINE):
            out.append(cur)
            cur = []
    if cur:
        out.append(cur)
    return [(c[0][1], c[-1][2], wrap(" ".join(t for t, _, _ in c))) for c in out]


def wrap(text):
    """Splits a caption into two lines of about equal length."""
    if len(text) <= LINE:
        return text
    spaces = [i for i, ch in enumerate(text) if ch == " "]
    mid = min(spaces, key=lambda i: abs(i - len(text) / 2))
    return text[:mid] + "\n" + text[mid + 1:]


def stamp(t):
    ms = round(t * 1000)
    return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"


def write_srt(path, timed):
    """timed: (start, end, text) in seconds of the final video. A caption stays
    up until the next one starts if the pause between them is short."""
    lines = []
    for n, (a, b, text) in enumerate(timed, 1):
        nxt = timed[n][0] if n < len(timed) else b + 1
        end = nxt - 0.05 if nxt - b < 0.6 else b + 0.3
        lines.append(f"{n}\n{stamp(a)} --> {stamp(end)}\n{text}\n")
    path.write_text("\n".join(lines), encoding="utf-8")


def main(spec_file):
    spec = json.loads((HERE / spec_file).read_text(encoding="utf-8"))
    tag = f"-{spec['name']}" if spec.get("name") else ""
    timing = json.loads((OUT / f"timing{tag}.json").read_text())
    voice_dir, clip_dir = OUT / f"voice{tag}", OUT / f"clips{tag}"
    max_speed = spec.get("max_speed", 1.7)
    clips, total, timed = [], 0.0, []
    captions = all((voice_dir / f"{s['id']}.words.json").exists() for s in spec["scenes"])
    for scene in spec["scenes"]:
        sid = scene["id"]
        if not (OUT / "raw" / f"{sid}.webm").exists():
            print(f"skipping {sid}: not recorded yet")
            continue
        clip, dur = scene_clip(sid, timing[sid], voice_dir, clip_dir, max_speed)
        clips.append(clip)
        if captions:
            words = json.loads((voice_dir / f"{sid}.words.json").read_text(encoding="utf-8"))
            timed += [(total + LEAD + a, total + LEAD + b, text) for a, b, text in cues(scene["say"], words)]
        # The clip's real length, so later scenes' captions don't drift.
        total += duration(clip)
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
    if captions:
        srt = OUT / f"tacit-demo{tag}.srt"
        write_srt(srt, timed)
        burned = OUT / f"tacit-demo{tag}-captions.mp4"
        # The subtitles filter can't take a Windows drive path, so run from OUT.
        res = subprocess.run([
            FF, "-y", "-loglevel", "error", "-i", final.name,
            "-vf", f"subtitles={srt.name}:force_style='{CAPTION_STYLE}'",
            "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-c:a", "copy", "-movflags", "+faststart", burned.name,
        ], cwd=OUT, capture_output=True, text=True)
        if res.returncode:
            raise SystemExit(res.stderr)
        print(f"{burned} ({len(timed)} captions)")


args = sys.argv[1:]
main(args[1] if args[:1] == ["--spec"] else "scenes.json")
