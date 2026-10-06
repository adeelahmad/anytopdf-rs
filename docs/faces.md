# Face detection

`anytopdf-plugin-faces` is a runtime plugin (protocol v1, `unit-enricher`) that
finds faces in images and video keyframes. It records only neutral facts: where
each face is, five landmark points, the detector's confidence and how many faces
a frame has. It never estimates age, gender, emotion, ethnicity or any other
trait.

## Turning it on

The plugin ships in the release archives' `plugins/` folder (`install.sh` puts
it in `~/.local/share/anytopdf/plugins`, Homebrew in
`$(brew --prefix anytopdf)/libexec/plugins`) and, like every bundled plugin, runs
only once `ANYTOPDF_PLUGIN_PATH` names that folder:

```bash
export ANYTOPDF_PLUGIN_PATH="$HOME/.local/share/anytopdf/plugins"
anytopdf convert meeting.mp4 photos/ -o searchable.pdf
```

From source:

```bash
cargo build --release -p anytopdf-plugin-faces
ANYTOPDF_PLUGIN_PATH="$PWD/target/release" anytopdf convert photo.jpg -o photo.pdf
```

It needs no download, Python or native library: the YuNet detector from the
OpenCV model zoo (MIT, 225 KiB) is compiled in and runs on the CPU through
[`tract`](https://github.com/sonos/tract), a pure-Rust ONNX runtime. A frame
takes tens of milliseconds. A missing or undecodable frame becomes a
`plugin.warning` and the PDF is still written.

## Settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `ANYTOPDF_FACES` | `on` | `off` skips detection while keeping the plugin installed |
| `ANYTOPDF_FACES_THRESHOLD` | `0.8` | minimum detector score, 0 to 1 |
| `ANYTOPDF_FACES_MIN_SIZE` | `20` | smallest face kept, in source pixels (shorter box side) |
| `ANYTOPDF_FACES_INPUT_SIZE` | `640` | longest detector input side; frames are never upscaled |
| `ANYTOPDF_FACES_MODEL` | embedded | path to another YuNet-compatible ONNX export |
| `ANYTOPDF_FACES_CROPS` | `on` | `off` skips writing aligned crops |

YuNet finds faces from about 10 to 300 pixels across at the detector input, so
raise `ANYTOPDF_FACES_INPUT_SIZE` for crowds in high-resolution photos.

## Annotations

Each face becomes one `face` annotation on its visual unit:

```json
{
  "kind": "face",
  "text": "face",
  "provider": "yunet",
  "confidence": 0.9323,
  "region": {"x": 0.348, "y": 0.1229, "width": 0.1756, "height": 0.2214},
  "time_range": {"start_seconds": 12.5, "end_seconds": 12.5},
  "attributes": {
    "face_index": "0",
    "landmarks": "[[0.3909,0.1989],[0.4827,0.2004],[0.4304,0.2433],[0.3879,0.2789],[0.4681,0.2805]]",
    "landmark_order": "right_eye,left_eye,nose_tip,mouth_right,mouth_left",
    "crop": "faces/<unit id>/face-0.png",
    "detector": "yunet-2026may"
  }
}
```

- `region` and `landmarks` are normalized to the frame (0 to 1, top-left
  origin). "Right" is the subject's right, which appears on the image's left.
- `face_index` orders faces by score within the unit.
- `crop`, when present, is a 112x112 PNG under the job workspace, aligned to
  the five-point template ArcFace-style recognizers expect, so a later
  recognition step can embed it without detecting again. The path is relative
  to the workspace, which is deleted after the job.
- `time_range` copies the unit's, so keyframe faces carry their timestamp.

A unit with at least one face also gets a count annotation with no region:
text `1 face` or `3 faces` and attribute `face_count`. Frames without faces get
no annotations. A unit that already has `yunet` face annotations is left alone.

Face annotations go into the PDF's invisible text layer and the embedded chunks
(searching for "faces" finds frames with faces), but crop paths and landmarks
do not.
