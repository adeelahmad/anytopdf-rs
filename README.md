<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/brand/banner-dark.png">
    <img alt="anytopdf: turn anything into a searchable PDF" src="docs/brand/banner-light.png" width="100%">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/adeelahmad/anytopdf-rs/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/adeelahmad/anytopdf-rs/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/adeelahmad/anytopdf-rs/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/adeelahmad/anytopdf-rs?sort=semver"></a>
  <a href="#license"><img alt="License: MIT or Apache-2.0" src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue"></a>
  <img alt="Rust 1.92" src="https://img.shields.io/badge/rust-1.92-orange?logo=rust">
  <img alt="macOS, Linux, Windows" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey">
  <a href="#mcp-server"><img alt="MCP server" src="https://img.shields.io/badge/MCP-server-8A2BE2"></a>
</p>

# anytopdf

**A private search engine for everything you keep. Photos, scans, video, voice
notes, chat exports, email and Office files become searchable PDFs that you can
search and question, offline, from one download.**

```console
$ anytopdf ~/inbox --index -o inbox.pdf
$ anytopdf search northwind
$ anytopdf ask inbox.pdf "How much was the Northwind receipt?"
```

Every page looks like the original. Underneath, an invisible text layer carries
what anytopdf read, heard and saw: OCR from flattened phone photos, Whisper
transcripts, captions, objects, scene tags, places, dates and links. So Ctrl+F,
Spotlight, `pdftotext`, `anytopdf search` and your RAG pipeline all find it. A
manifest with SHA-256 hashes and per-page chunks is embedded in the PDF, which
makes the file its own index.

<p align="center">
  <img alt="An angled phone photo of a receipt becomes a flattened PDF page in which a search for 'total' highlights the words" src="docs/demo/before-after.png" width="100%">
</p>

<p align="center">
  <img alt="Terminal: anytopdf converts a folder with a receipt photo and a WhatsApp chat into an indexed PDF, anytopdf search finds Northwind in both, and anytopdf ask returns the passages with the total" src="docs/demo/demo.gif" width="80%">
</p>

<sub>Both images come from a real run of the release binary
(`python3 docs/demo/record.py`); nothing is mocked.</sub>

## What it does

| | |
| --- | --- |
| **Reads almost anything** | Photos (JPEG, PNG, TIFF, HEIC/AVIF, camera RAW) with phone-photo page flattening, scanned and digital PDFs, video, audio, SRT/VTT captions, text and Markdown, JSON and JSON Lines, HTML, web pages and YouTube links, browser bookmarks, `.eml`/`.mbox` email with attachments, WhatsApp/Telegram/Slack/iMessage chat exports, Word/Excel/PowerPoint/OpenDocument, zip and tar archives, screen recordings and print jobs |
| **Reads, hears and sees** | OCR through Apple Vision, docTR or Tesseract, word-aligned under the image; Whisper speech to text; CLIP scene tags so "beach" finds beach photos; YOLO objects; face presence and opt-in matching against your own local face index; laughter, applause and sirens; dominant colours, places, dates, links, sentiment and tone. Nothing estimates age, gender or emotion |
| **Search and ask across everything** | `anytopdf search` queries a local SQLite index of every converted PDF, filtered by kind or person; `anytopdf ask` returns ranked passages citing file, page and time, and writes a cited answer with any local OpenAI-compatible LLM |
| **Writes a real archive file** | Tagged PDF/A-3a with bookmarks, Arabic/Hebrew/CJK shaping, byte-reproducible output, and a provenance page |
| **Proves where it came from** | Embedded `anytopdf-manifest.json` and `anytopdf-chunks.json` with source hashes, page maps and entities; `anytopdf extract --json` reads them back |
| **Plugs into agents** | `anytopdf mcp` is a Model Context Protocol server with convert, search and ask tools: `claude mcp add anytopdf -- anytopdf mcp` |
| **Runs unattended** | Folder-backed job queue, HTTP upload intake, signed webhooks, an IMAP mailbox watcher, and an optional IPP printer helper that phones and laptops can print to |
| **Extends without forks** | Any `anytopdf-plugin-*` executable on `PATH`, in any language, speaks a versioned JSON protocol; an opt-in sandbox confines it |

Nothing leaves your machine: models run locally, there is no telemetry, and the
network is used only when you convert a URL, download a model, point anytopdf at
your own LLM endpoint, or turn on an intake channel (queue server, webhooks, IMAP
or the printer).

## Install

| Platform | Command |
| --- | --- |
| macOS, Linux | `curl -fsSL https://raw.githubusercontent.com/adeelahmad/anytopdf-rs/main/install.sh \| sh` |
| Homebrew | `brew tap adeelahmad/anytopdf https://github.com/adeelahmad/anytopdf-rs && brew install adeelahmad/anytopdf/anytopdf` |
| Windows (Scoop) | `scoop bucket add anytopdf https://github.com/adeelahmad/anytopdf-rs; scoop install anytopdf/anytopdf` |
| cargo-binstall | `cargo binstall --git https://github.com/adeelahmad/anytopdf-rs anytopdf` |
| Docker | `docker run --rm -v "$PWD:/work" ghcr.io/adeelahmad/anytopdf-rs photo.jpg -o photo.pdf` |
| From source | `cargo install --locked --git https://github.com/adeelahmad/anytopdf-rs anytopdf` |

Or download an archive for your platform from
[Releases](https://github.com/adeelahmad/anytopdf-rs/releases/latest). The
installer verifies the release's SHA-256 checksum before it installs anything.
Text and image conversion need nothing else; run `anytopdf doctor` to see which
optional providers (FFmpeg, Tesseract, ExifTool, Poppler, LibreOffice) it found.
Every channel is described in [docs/distribution.md](docs/distribution.md).

## Quick start

Install as above, or unpack the archive for your operating system and run `./anytopdf` (Windows:
`anytopdf.exe`). The executable needs no Rust or Python installation for text and
image conversion; provider-specific dependencies are listed below.

```bash
./anytopdf --version
./anytopdf notes.txt photo.jpg -o out.pdf
./anytopdf --no-plugins convert notes.txt --ocr off -o notes.pdf
./anytopdf doctor
```

Keep `Cargo.lock` when building from source. For video, install FFmpeg; for OCR,
use native Apple Vision on macOS or install Tesseract. Audio transcription requires
a supplied transcript or a plugin.

Office documents (`.docx`, `.xlsx`, `.pptx`, `.odt`, `.ods`, `.odp`, `.rtf` and
their legacy formats) need LibreOffice (`soffice`) and Poppler's `pdftoppm`.
Each page is rendered as an image; with Poppler's `pdftotext` the document's own
text is placed invisibly over it and OCR is skipped for those pages. Without
`pdftotext`, pages fall back to OCR. The conversion runs in the job workspace with
a private LibreOffice profile, so the original file is never opened in place.

### Whisper transcription

Turn on speech-to-text in one step:

```bash
anytopdf setup whisper              # downloads ggml-base.bin (142 MiB), checks its SHA-1
anytopdf convert meeting.mp4 -o meeting.pdf
```

`anytopdf-plugin-whisper` transcribes audio and video sources that have no
sidecar or `--transcript` transcript. It extracts the audio with FFmpeg and runs
a Whisper engine, adding a visible, timed transcript page whose segments are
searchable `transcript` annotations.

`setup whisper` installs a whisper.cpp model into anytopdf's data folder
(`ANYTOPDF_DATA_DIR`, or `~/Library/Application Support/anytopdf` on macOS,
`%LOCALAPPDATA%\anytopdf` on Windows, `~/.local/share/anytopdf` elsewhere) and
records it. Pick another model with `--model` (`tiny`, `base.en`, `small`,
`large-v3-turbo`, …; `.en` models are English-only), install a file you already
downloaded with `--from FILE`, or use a mirror with `--base-url URL`. Every file
is checked against the checksum whisper.cpp publishes.

The plugin ships with anytopdf (in `plugins/` beside the binary, or Homebrew's
`libexec/plugins`) and turns itself on once all three of these are present:

- a model: the one `setup whisper` recorded, or `ANYTOPDF_WHISPER_MODEL` set to
  a ggml model file;
- an engine: whisper.cpp's `whisper-cli` (`brew install whisper-cpp`; on Windows
  `whisper-cli.exe` from the whisper.cpp release zip), or `whisper-ctranslate2`
  (faster-whisper) / OpenAI `whisper` with `ANYTOPDF_WHISPER_MODEL` naming the
  model (default `base`);
- FFmpeg on `PATH`.

Until then it stays off, so media conversions don't warn. `anytopdf doctor` (and
`setup whisper` itself) says exactly which piece is missing. A plugin you put on
`PATH` or `ANYTOPDF_PLUGIN_PATH` always runs and reports what it lacks as a
warning. The container image has a `WHISPER=cpp` build that includes whisper.cpp
(see [docs/distribution.md](docs/distribution.md)). From source:

```bash
cargo build --release -p anytopdf -p anytopdf-plugin-whisper
mkdir -p target/release/plugins && cp target/release/anytopdf-plugin-whisper target/release/plugins/
target/release/anytopdf setup whisper
target/release/anytopdf convert meeting.mp4 --plugin-timeout 1800 -o meeting.pdf
```

`ANYTOPDF_WHISPER_ENGINE` (`auto`, `whisper.cpp`, `openai-whisper`),
`ANYTOPDF_WHISPER_BIN` (an engine executable not on `PATH`) and
`ANYTOPDF_WHISPER_LANGUAGE` (default: detect) override the defaults. One plugin
invocation transcribes every media source in the job, so raise
`--plugin-timeout` (default 60 seconds) for long recordings. A missing engine,
missing FFmpeg or a source without an audio track becomes a `plugin.warning`;
the PDF is still written. Under `--plugin-sandbox strict`, pass
`--plugin-sandbox-allow-read` for the data folder (or the model file) so the
plugin can load the model.

### Face detection

`anytopdf-plugin-faces` finds faces in images and video keyframes with a
compiled-in YuNet detector, so it needs no download or Python. Each face becomes
a searchable `face` annotation with its box, five landmarks and confidence, plus
a per-frame count (`2 faces`). It never estimates age, gender, emotion or other
traits. It ships beside the Whisper plugin and is enabled the same way:

```bash
cargo build --release -p anytopdf-plugin-faces
ANYTOPDF_PLUGIN_PATH="$PWD/target/release" anytopdf convert meeting.mp4 -o meeting.pdf
```

`ANYTOPDF_FACES=off`, `ANYTOPDF_FACES_THRESHOLD`, `ANYTOPDF_FACES_MIN_SIZE` and
the annotation format are described in [docs/faces.md](docs/faces.md).

### Search by meaning (CLIP)

`anytopdf-plugin-clip` embeds every image, document page and video keyframe with
OpenAI's CLIP ViT-B/32, so they can be searched by what they show rather than by
the words on them. It runs on the CPU in pure Rust; no Python or ONNX Runtime
install is needed. It also adds zero-shot scene tags (`photo`, `document`,
`screenshot`, `slide`, `chart`, `indoors`/`outdoors`, `beach`, `city street`,
`office`, `food`, `night` and similar) to the PDF's searchable layer, so searching
the PDF for "screenshot" or "beach" finds those pages. The tags describe content
and setting only, never a person's traits.

```bash
cargo build --release -p anytopdf-plugin-clip
target/release/anytopdf-plugin-clip --fetch-model     # about 600 MB, checksum-verified
export ANYTOPDF_PLUGIN_PATH="$PWD/target/release"
anytopdf convert holiday/ -o holiday.pdf
```

`--fetch-model [DIR]` stores the model in `anytopdf/models/clip` in the user data
directory (`ANYTOPDF_CLIP_MODEL_DIR` overrides it; `ANYTOPDF_CLIP_MODEL_MIRROR` names
a mirror holding the same three files). A Hugging Face ONNX export
(`onnx/vision_model.onnx`, `onnx/text_model.onnx`, `tokenizer.json`) also works.
`ANYTOPDF_CLIP_TAGS=off` turns tags off, `ANYTOPDF_CLIP_TAGS="logo=a company logo;cat"`
replaces them with your own labels, and `ANYTOPDF_CLIP_TAG_THRESHOLD` (default 0.5)
sets how sure a tag must be. Each embedding stays in the conversion's graph
(`clip.embedding`) for the cross-file search index and is never written into the
PDF. `anytopdf-plugin-clip --encode-text "people on a beach at night"` and
`--encode-image photo.jpg` print a query embedding as JSON. Without a model the
plugin warns once per conversion and the PDF is still written. Under
`--plugin-sandbox strict`, allow the model folder with
`--plugin-sandbox-allow-read "$ANYTOPDF_CLIP_MODEL_DIR"` (or the default folder).

### Sentiment and tone

`anytopdf-plugin-sentiment` labels what text *says* as `positive`, `negative` or
`neutral`, and tags its tone as `question`, `complaint` or `urgent` (plus `formal`
or `informal` with an LLM). It reads every text source: transcript segments
(including Whisper's), caption cues, OCR blocks (words grouped into lines and
paragraphs) and the paragraphs of plain-text, email and document units. Each label
is a `custom` annotation with `entity` set to `sentiment` or `tone`, carrying the
segment's time range or OCR region; a unit with several segments also gets one
`sentiment-overall` line such as "overall negative", weighted by segment length.
Searching the PDF for "negative" or "complaint" finds those moments. Labels never
come from faces or voices and are never attached to a person.

It needs no model: by default it scores English text with a built-in port of the
VADER lexicon. For other languages and better accuracy, point it at a local
OpenAI-compatible endpoint (Ollama, llama.cpp `llama-server`, LM Studio, vLLM); if
the endpoint fails, the lexicon is used and a `plugin.warning` says so.

```bash
cargo build --release -p anytopdf-plugin-sentiment
export ANYTOPDF_PLUGIN_PATH="$PWD/target/release"
anytopdf convert support-call.mp4 -o call.pdf

# Optional: a local LLM instead of the lexicon
export ANYTOPDF_SENTIMENT_LLM_URL=http://127.0.0.1:11434/v1
export ANYTOPDF_SENTIMENT_LLM_MODEL=qwen2.5:3b
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `ANYTOPDF_SENTIMENT_BACKEND` | `auto` | `vader`, `llm`, or `auto` (`llm` when an LLM URL is set) |
| `ANYTOPDF_SENTIMENT_LLM_URL` | `ANYTOPDF_LLM_URL` | OpenAI-compatible base URL or full `/chat/completions` URL |
| `ANYTOPDF_SENTIMENT_LLM_MODEL` | `ANYTOPDF_LLM_MODEL` | Model name, required with the LLM backend |
| `ANYTOPDF_SENTIMENT_LLM_API_KEY` | `ANYTOPDF_LLM_API_KEY` | Sent as a bearer token |
| `ANYTOPDF_SENTIMENT_LLM_TIMEOUT` | `45` | Seconds per request (24 segments each) |
| `ANYTOPDF_SENTIMENT_FROM` | all | Comma list of `transcript`, `caption`, `ocr`, `text` |
| `ANYTOPDF_SENTIMENT_NEUTRAL` | `false` | Also annotate neutral segments |
| `ANYTOPDF_SENTIMENT_TONES` | `true` | Add tone tags |
| `ANYTOPDF_SENTIMENT_THRESHOLD` | `0.05` | Lexicon score needed for positive or negative |

A request `options` object (how anytopdf's layered configuration passes a
`[sentiment]` table) takes precedence: `llm_model = "qwen2.5:3b"` there wins over
`ANYTOPDF_SENTIMENT_LLM_MODEL`, and lists such as `from = ["ocr"]` are accepted.

The plugin runs once per unit; with an LLM, raise `--plugin-timeout` for long
transcripts. `--plugin-sandbox strict` blocks network access, so the LLM backend
falls back to the lexicon there.

### Audio events

`anytopdf-plugin-audio-events` adds an "Audio events" page to every audio and
video source: a compact timeline (`00:12 applause · 03:40 music · 05:02 laughter`)
and every event with its time range. Each event is also a `custom` annotation
with `entity = audio-event`, a `label`, a time range and, from a model, a
confidence, so it lands in the chunks JSON. It decodes the audio with FFmpeg and
looks at it one second at a time:

- **Without a model** it reports `speech`, `music`, `noise` and `silence`
  segments from the signal itself (level, low-energy and zero-crossing
  patterns), and `raised-voice` where the BS.1770 loudness of speech is at
  least `ANYTOPDF_AUDIO_EVENTS_RAISED_LU` (default 8) LU above the file's
  median speech level. That is a measured level jump, not a guess at anyone's
  mood.
- **With an AudioSet model** (YAMNet, PANNs CNN14 or any ONNX export that takes
  a 16 kHz waveform) it also reports `laughter`, `applause`, `cheering`,
  `singing`, `crying-baby`, `dog`, `siren`, `alarm`, `gunshot`, `explosion`,
  `vehicle`, `car-horn`, `door`, `knock`, `keyboard-typing`, `telephone` and
  `glass-breaking`, and the model decides speech versus music. Set
  `ANYTOPDF_AUDIO_EVENTS_MODEL` to the `.onnx` file and
  `ANYTOPDF_AUDIO_EVENTS_LABELS` to its class map CSV (found automatically as
  `<model>_class_map.csv` or `class_labels_indices.csv` beside it). ONNX
  Runtime is loaded at run time from `ORT_DYLIB_PATH` or the system library
  path; `ANYTOPDF_AUDIO_EVENTS_DEVICE` picks `cpu` (default), `cuda` or
  `coreml`, and `ANYTOPDF_AUDIO_EVENTS_THRESHOLD` (default 0.3) the minimum
  score.

The plugin never labels how a person feels: AudioSet classes such as crying,
sobbing or screaming by adults are not reported, and there is no voice emotion,
age or gender analysis. It ships beside the Whisper plugin and is turned on the
same way (`ANYTOPDF_PLUGIN_PATH`). The Linux release binaries are static and
cannot load ONNX Runtime, so for a model on Linux build the plugin from source
(`cargo build --release -p anytopdf-plugin-audio-events`); signal analysis works
in every build. A missing or broken model becomes a `plugin.warning` and the
signal results are still written.

### Face recognition

`--recognize-faces` names the people in photos and video keyframes from a local
face index, so searching the PDF for "Alice" finds every frame she is in, and
adds a "People in …" page per source listing each person with the times (or
pages) where they appear. Faces that match nobody are grouped as `person-N`
across runs; name a group once and later runs use the name. It is off by
default and needs two runtime plugins: a face detector that adds `face`
annotations with five-point landmarks, and `anytopdf-plugin-face-id`, which
embeds each face with an ArcFace-style ONNX model (for example InsightFace
`w600k_mbf.onnx` or `w600k_r50.onnx`; check the model's licence) using pure-Rust
inference, so no Python or ONNX Runtime install is needed. OpenCV SFace and the
ONNX-zoo ArcFace R100 work too: models that scale their own input get raw 0-255
pixels, others -1 to 1 (`ANYTOPDF_FACE_EMBED_INPUT=raw|normalized` overrides).

```bash
cargo build --release -p anytopdf-plugin-face-id
export ANYTOPDF_PLUGIN_PATH="$PWD/target/release"   # plus the face detector
export ANYTOPDF_FACE_EMBED_MODEL="$HOME/models/w600k_mbf.onnx"

anytopdf faces enroll Alice alice-1.jpg alice-2.jpg
anytopdf faces import faces/              # faces/<Name>/*.jpg, one folder per person
anytopdf convert party.mp4 --recognize-faces -o party.pdf
anytopdf faces list                       # Alice, person-2, person-3 …
anytopdf faces name person-2 Bob          # merges into Bob if Bob already exists
anytopdf faces find someone.jpg           # every file and time that face appears
anytopdf faces rename|merge|forget …
```

The index is `faces.sqlite` in the anytopdf data directory
(`$XDG_DATA_HOME/anytopdf`, `~/Library/Application Support/anytopdf` or
`%LOCALAPPDATA%\anytopdf`; override with `ANYTOPDF_DATA_DIR`, `ANYTOPDF_FACE_INDEX` or
`--face-index`). It holds embeddings and sightings, which never go into a PDF.
`--face-threshold` (default 0.40, cosine similarity) sets how close a face must
be to count as a known person; two faces in one frame are never the same
person. Only matches at 0.60 or above are remembered as more examples of that
person, so a borderline match never pulls later faces toward the wrong person. Embedding runs as one plugin call per conversion, so raise
`--plugin-timeout` for long videos. `--profile share` leaves names and the
"People in …" pages out of the PDF. Identities come only from people you enroll
or name: nothing infers age, gender, emotion or other traits.

### Visual descriptions

`anytopdf-plugin-vlm` asks a local vision-language model about every image and
video keyframe and writes the answers as searchable `caption` annotations. By
default it asks for a caption, "What do you see in this image?" and the
activities shown, as the original video-analysis script did. For each video,
audio file or subtitle file it then adds a visible summary page before the
source's other pages, with a category and up to five topics (`custom`
annotations whose `entity` is `category` or `topic`). Videos also get a one- or
two-sentence summary per scene (keyframes are split at FFmpeg scene changes).
Summaries are written from the keyframe descriptions plus any transcript or
subtitles; audio needs a transcript, for example from the Whisper plugin. Prompts
never ask for anyone's age, gender, ethnicity or emotions.

The plugin does nothing until `ANYTOPDF_VLM_URL` is set. It talks to either:

- an OpenAI-compatible endpoint (default `ANYTOPDF_VLM_ENGINE=openai`): Ollama,
  llama.cpp `llama-server`, LM Studio or vLLM, with `ANYTOPDF_VLM_MODEL` naming
  a vision model; or
- the Moondream API (`ANYTOPDF_VLM_ENGINE=moondream`): Moondream Station, or
  Moondream2 through transformers with the bundled
  `helpers/anytopdf-moondream-server.py` (`--device cpu|cuda`). This engine also
  supports open-vocabulary detection: `ANYTOPDF_VLM_DETECT="red car,logo"` adds
  an `object` annotation with a box for each match.

```bash
ollama pull llava
cargo build --release -p anytopdf-plugin-vlm
export ANYTOPDF_PLUGIN_PATH="$PWD/target/release"
export ANYTOPDF_VLM_URL=http://127.0.0.1:11434/v1 ANYTOPDF_VLM_MODEL=llava
anytopdf convert meeting.mp4 --plugin-timeout 120 -o meeting.pdf
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `ANYTOPDF_VLM_PROMPTS` | caption, query, activity | `type=prompt` entries separated by `\|`; an entry without `type=` is a `query` |
| `ANYTOPDF_VLM_TIMEOUT` | `50` | seconds per keyframe, and for all summaries of a run; keep it below `--plugin-timeout` |
| `ANYTOPDF_VLM_MAX_SIDE` | `768` | images are downscaled to this many pixels before upload |
| `ANYTOPDF_VLM_API_KEY` | none | bearer token (OpenAI) or `X-Moondream-Auth` (Moondream) |
| `ANYTOPDF_VLM_SUMMARY` | `on` | `off` skips summaries and categories |
| `ANYTOPDF_LLM_URL`, `ANYTOPDF_LLM_MODEL` | the VLM endpoint | OpenAI-compatible text model for summaries; required for them with the Moondream engine |
| `ANYTOPDF_VLM_CATEGORIES` | news, entertainment, education, sports, music, gaming, tutorial, meeting, documentary, other | comma-separated category list |

Images go to the configured endpoint, so point it at a server you trust; a
loopback URL bypasses any HTTP proxy. `--plugin-sandbox strict` blocks network
access and therefore this plugin. An unreachable endpoint, a slow frame or a bad
answer becomes a `plugin.warning`, and the PDF is still written.

### Object detection

`anytopdf-plugin-objects` runs a YOLO object detector on every image and video
keyframe, including photos attached to emails or packed in archives. Each detection becomes a searchable `object` annotation with its label,
confidence, normalized box and (for video) the frame's time, and each frame also
gets a count such as `objects: 4 person, 1 bus`, so searching the PDF for "dog"
finds the frames with a dog. Pages of PDF, Office and HTML documents are skipped.

Inference runs on [tract](https://github.com/sonos/tract), a pure-Rust ONNX
runtime, so the plugin needs no Python, CUDA or native library. It takes a YOLOv8
or YOLO11 ONNX export (YOLOv5 exports work too); `yolo11n.onnx` from the
[Ultralytics assets release](https://github.com/ultralytics/assets/releases/tag/v8.3.0)
is a good default and labels the 80 COCO classes. Ultralytics models are AGPL; for
an Apache-2.0 model use a YOLOX export, such as `yolox_s.onnx` from the
[YOLOX releases](https://github.com/Megvii-BaseDetection/YOLOX/releases) or
`object_detection_yolox_2022nov.onnx` from the
[OpenCV model zoo](https://github.com/opencv/opencv_zoo/tree/main/models/object_detection_yolox).
The head type is recognised from the model's output shape; a model that is none of
these, or whose class count does not match its labels, is reported as a warning
instead of silently finding nothing. From source:

```bash
cargo build --release -p anytopdf-plugin-objects
export ANYTOPDF_PLUGIN_PATH="$PWD/target/release"
export ANYTOPDF_OBJECTS_MODEL="$HOME/models/yolo11n.onnx"
anytopdf convert meeting.mp4 --plugin-timeout 600 -o meeting.pdf
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `ANYTOPDF_OBJECTS_MODEL` | none (required) | YOLO `.onnx` file |
| `ANYTOPDF_OBJECTS_LABELS` | model's `names`, else COCO | text file, one class name per line |
| `ANYTOPDF_OBJECTS_CONFIDENCE` | `0.25` | minimum score kept |
| `ANYTOPDF_OBJECTS_IOU` | `0.45` | overlap above which same-class boxes merge |
| `ANYTOPDF_OBJECTS_CLASSES` | all | comma-separated labels to keep, e.g. `person,car` |
| `ANYTOPDF_OBJECTS_INPUT_SIZE` | model's, else `640` | input size for models with dynamic shapes |
| `ANYTOPDF_OBJECTS_MAX` | `100` | most detections per frame |
| `ANYTOPDF_OBJECTS_DEVICE` | `cpu` | only `cpu`; other values warn and use the CPU |

The model loads once per job and `yolo11n` takes well under a second per frame on
a CPU, so raise `--plugin-timeout` (default 60 seconds) for
long videos. A missing model, an unreadable frame or a bad setting becomes a
`plugin.warning`; the PDF is still written.

## How it works

`anytopdf` is a pluggable media/document ingestion engine whose canonical output
is a searchable, RAG-friendly PDF.

It is deliberately not implemented as "a list of file extensions plus converters".
The architecture is closer to a driver framework:

```
sources
  -> discover/probe
  -> importer
  -> asset graph
  -> source enrichers
  -> unit enrichers
  -> page planner
  -> renderer
  -> searchable PDF
```

A source may produce any number of derived units. A video can produce keyframes,
audio segments, subtitle cues and metadata. An image generally produces one visual
unit. A transcript produces text units. Future importers can produce whatever
representation makes sense for formats such as `.igl`, CAD, email, Office,
archives, proprietary exports, or remote sources.

## Search model

Every fact becomes an `Annotation` with provenance:

- OCR text and bounding boxes
- captions / closed captions
- transcript text and time ranges
- file / EXIF / XMP / ffprobe metadata
- date/time and GPS/location metadata
- scene/keyframe changes
- object labels from an object-analysis plugin
- face presence, count and bounds from a face-analysis plugin, and the names of
  people you enrolled (`--recognize-faces`)
- arbitrary future annotations

The PDF renderer paints the visual page normally and emits searchable annotations
as invisible text (fill opacity 0 in the default `pdfa` renderer, text rendering
mode 3 in `pdf`). The hidden layer carries content only
(OCR, captions, transcripts, objects, faces, barcodes, colours, time ranges, and place
names read from that text); source paths and file metadata, including GPS-derived
places, are never written into it. Text/transcript units become normal
visible text pages.

Face enrichment is limited to presence, count and bounds, plus identity only
for people the user enrolls or names in the local face index. It does **not**
infer gender identity, emotion, age or other sensitive/demographic traits from
a face. The plugin model supports
adding other non-sensitive semantic analyzers without changing the core.

## Plugin model

There are two kinds of plugins.

### 1. Built-in Rust plugins

These implement Rust traits and are compiled into the executable. This is what
you use for fast, portable core functionality.

### 2. Runtime executable plugins

Executables named `anytopdf-plugin-*` are discovered from `PATH` and directories
in `ANYTOPDF_PLUGIN_PATH` (using the platform path separator). They speak the versioned JSON protocol documented in
`PLUGIN_PROTOCOL.md`.

This is the compatibility boundary for future/proprietary formats. A runtime
plugin can be written in Rust, Go, Python, Swift, C++, etc.; there is no Rust
dynamic-library ABI dependency.

A future `.igl` plugin can therefore be shipped independently:

```
anytopdf-plugin-igl
```

and register itself as an importer without modifying the core binary.

## Current built-ins

Importers:
- raster images
- existing PDFs: pages rendered by Poppler `pdftoppm` with their own text layer kept (OCR only for textless pages); text only without Poppler
- HEIC/HEIF/AVIF photos, converted by `sips` (macOS), `heif-convert` (libheif) or ImageMagick
- camera RAW photos (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF and more): the embedded
  camera preview, found without any external tool; when it is missing or smaller than
  1600 px on its long edge the RAW data is developed by `sips` (macOS), LibRaw's
  `dcraw_emu`, `dcraw` or ImageMagick (`--raw-decode auto|preview|develop`)
- PWG Raster and Apple Raster (URF) print jobs
- video through FFmpeg
- audio container placeholder units
- text / Markdown
- JSON and JSON Lines (`.json`, `.jsonl`, `.ndjson`, or sniffed): one searchable chunk per record with its key paths, API envelopes such as `{"data": [...]}` split into records, other documents as an indented outline
- HTML pages (readable text, title and image alt text; no network fetches). With
  `--html-render` (`[html] render = true`) headless Chrome, Chromium or Edge prints the
  page offline (no remote images, fonts or scripts) and each printed page becomes an
  image page with the page's own text as its searchable layer; without a browser or
  Poppler `pdftoppm` the file falls back to readable text with a warning
- email (`.eml`, `.mbox`): headers and body as text; attachments imported by their own importers
- chat exports: WhatsApp (`_chat.txt` or the exported `.zip`), Telegram Desktop JSON
  (`result.json`, one chat or a whole account), Slack workspace exports (the `.zip`, or
  extracted `YYYY-MM-DD.json` day files) and iMessage text from `imessage-exporter -f txt`.
  Each conversation becomes text pages with a `time sender: text` line per message under
  date headings; photos and files shipped inside the export are imported right after the
  message that sent them (never from outside the export's folder, and once even when the
  whole export folder is converted). Slack exports only link files, so those stay named
  in the text. `--chat-date-order auto|dmy|mdy|ymd` reads
  ambiguous WhatsApp dates; `--no-chat-attachments` names attachments without importing them
- archives (`.zip`, `.tar`, `.tar.gz`/`.tgz`): an index page plus every member through its own importer, with zip-bomb and path-traversal limits
- SRT / VTT captions
- Office documents (Word, Excel, PowerPoint, OpenDocument, RTF) through
  LibreOffice and Poppler

Enrichment:
- photographed pages: the sheet is found, perspective-corrected and deskewed before OCR
  (`--scan-mode auto|on|off`, default `auto`)
- ExifTool metadata
- ffprobe media metadata
- OCR provider chain:
  - Apple Vision on macOS when built with `apple-vision`
  - docTR through local Python when available
  - Tesseract CLI
- sidecar captions / transcripts
- video timestamps and scene-selection provenance
- URLs, email addresses, domains, app names, dates and times from OCR, captions and
  transcripts alike (`--no-entities` turns this off). App names come from a small
  gazetteer, window titles such as `Budget.xlsx - Excel`, and URL domains
  (`docs.google.com/spreadsheets` is Google Sheets), never from bare capitalized
  words. Dates ("3 March 2024", "2024-03-03 14:05", "Tuesday at 5pm", "last
  Friday") are normalized to ISO 8601; relative ones resolve against the capture
  date from ExifTool or ffprobe when known, and each keeps the moment it was seen
  or spoken. `--date-order dmy|mdy` (default `dmy`) reads `03/04/2024`
- dominant colours of images and keyframes (up to five per page, named
  "red", "navy blue", ... with hex and share), so searching a colour finds
  the frames it dominates; `--colors off` disables it
- location (`--location on|gps|off`, default `on`): the source's GPS fix (EXIF,
  XMP, QuickTime `GPSCoordinates` or ISO 6709 `location` tags) becomes one
  `location` annotation on its first unit, reverse geocoded offline to
  "City, Region, Country" from an embedded GeoNames `cities1000` table. Place names
  in OCR text, captions, transcripts and text pages (cities of 100,000 people or
  more, countries, and a few aliases such as "USA" and "England") become
  `location` annotations with `attributes.source = text` and are searchable in the
  PDF. GPS-derived locations stay out of the hidden layer and `--profile share`
  drops them; place names read from the text are kept.

Rendering:
- tagged PDF/A-3a with bookmarks via `krilla` (default, `--renderer pdfa`)
- plain searchable PDF via `printpdf` (`--renderer pdf`)

Bundled runtime plugins (separate executables in this workspace):
- `anytopdf-plugin-faces`: neutral face boxes, landmarks and counts for images
  and video keyframes with an embedded YuNet detector
- `anytopdf-plugin-whisper`: speech-to-text for audio and video through
  whisper.cpp or an OpenAI-compatible Whisper CLI
- `anytopdf-plugin-clip`: CLIP image embeddings for search by meaning, plus
  zero-shot scene tags in the searchable layer
- `anytopdf-plugin-objects`: YOLO object detection on images and video keyframes
  through a pure-Rust ONNX runtime
- `anytopdf-plugin-sentiment`: sentiment and tone of transcript, caption, OCR
  and document text, from a built-in English lexicon or a local LLM endpoint
- `anytopdf-plugin-audio-events`: speech, music, silence and raised-voice
  segments, plus laughter, applause and other sound events with an AudioSet
  ONNX model

- `anytopdf-plugin-face-id`: face embeddings for `--recognize-faces`, through an
  ArcFace-style ONNX model
- `anytopdf-plugin-vlm`: keyframe captions, questions, activities, video and
  scene summaries and a category through a local vision-language model

External plugins are the intended route for model-heavy enrichers such as:
- DETR / open-vocabulary object detection
- speech-to-text engines
- format-specific decoders
- proprietary document systems

## Printing to anytopdf

`helpers/anytopdf-printer` is an optional IPP Everywhere printer built on
[PAPPL](https://www.msweet.org/pappl/) for Linux and macOS. Anything that can print
(macOS, iOS, Windows, Android, CUPS) can print to it, and every job becomes a
searchable PDF in an output folder. It listens on localhost unless told otherwise:

```bash
make printer
ANYTOPDF_BIN=target/release/anytopdf \
  helpers/anytopdf-printer/anytopdf-printer server -o output-directory=$HOME/Printed
```

The helper only spools pages; `anytopdf convert` does the work, so a saved
`job.pwg` or `job.urf` print job converts the same way on any platform.

## CLI

```bash
anytopdf convert . -o archive.pdf
anytopdf convert photo.jpg meeting.mp4 transcript.srt -o searchable.pdf
anytopdf convert meeting.mp4 --transcript meeting.vtt -o meeting.pdf
anytopdf convert . --filter 'invoice|receipt' -o receipts.pdf
anytopdf convert https://example.com/post -o post.pdf
anytopdf convert 'https://www.youtube.com/watch?v=…' -o talk.pdf

anytopdf doctor
anytopdf setup whisper
anytopdf plugins
anytopdf probe some.igl
anytopdf extract archive.pdf --json
anytopdf capture screen --duration 60 -o screen.pdf
anytopdf ask archive.pdf "When is the Acme invoice due?"
anytopdf mcp
```

Video defaults combine interval sampling and FFmpeg scene-change sampling and
then perceptually deduplicate frames.

```bash
anytopdf convert meeting.mp4 \
  --video-interval 5 \
  --scene-threshold 0.30 \
  -o meeting.pdf
```

### URL inputs

Any `http://` or `https://` argument is downloaded before discovery and then handled
by the normal importers:

- **Web pages** become readable text (the page's `<main>` or single `<article>`
  when it marks one, else the whole body), followed by a snapshot printed by headless
  Chrome, Chromium or Edge. `--url-snapshot auto` (default) takes the snapshot when
  such a browser and Poppler `pdftoppm` are installed; `on` always tries, `off` never.
  Set `ANYTOPDF_CHROME` to choose the browser.
- **Video and podcast links** on known hosts (YouTube, Vimeo, SoundCloud, Apple
  Podcasts and others) are fetched with [yt-dlp](https://github.com/yt-dlp/yt-dlp)
  together with their captions (`--url-sub-langs`, default `en.*,en`), which become
  timed caption annotations, and the video's chapters tag the frames inside them
  (`chapter: …` scene annotations). Audio without captions needs the Whisper plugin for a
  transcript. Set `ANYTOPDF_YT_DLP` to choose the yt-dlp executable.
- **Anything else** (PDFs, images, audio, text) is saved with an extension from its
  URL or `Content-Type` and probed like a local file.

`--links FILE` (repeatable) converts every link in a list: a text file with one URL
per line (text after the URL is its title, `#` starts a comment), a browser bookmark
export (Chrome, Edge, Firefox or Safari "Export bookmarks" HTML) or Chrome's profile
`Bookmarks` JSON. Bookmark folders become nested PDF bookmarks labelled with the
bookmark titles, the default output is named after the list (`bookmarks.pdf`), and an
unreachable link is skipped with `input.unreadable` instead of failing the run.

```bash
anytopdf convert --links reading-list.txt
anytopdf convert --links ~/Downloads/bookmarks_10_6_26.html -o bookmarks.pdf
```

`--url-mode page|media` forces a plain download or yt-dlp for every URL. Each source
records `url.source`, `url.fetched` and, for redirects, `url.final` in the manifest
(dropped by `--profile share`), and the provenance page lists the URL. The default
output is named after the URL, e.g. `example.com-post.pdf`.

Downloads are capped by `--url-max-mb` (default 1024) and `--url-timeout` seconds
(default 600). URLs on loopback, private and link-local addresses are refused unless
`--url-allow-private` is given. Every URL option can also be set through its
environment variable (`ANYTOPDF_URL_MODE`, `ANYTOPDF_URL_SNAPSHOT`,
`ANYTOPDF_URL_SUB_LANGS`, `ANYTOPDF_URL_MAX_HEIGHT`, `ANYTOPDF_URL_MAX_MB`,
`ANYTOPDF_URL_TIMEOUT`, `ANYTOPDF_URL_ALLOW_PRIVATE`).

### Configuration file

Every option has three equivalent spellings, rclone-style, all derived from one
definition: `interval` under `[video]` in the config file is
`ANYTOPDF_VIDEO_INTERVAL` in the environment and `--video-interval` on
`anytopdf convert`. `anytopdf convert --help` lists every option grouped by type
(Video importer, OCR enricher, …) with its environment variable and default.
Later layers win:

1. built-in defaults
2. `~/.config/anytopdf/config.toml` (`$XDG_CONFIG_HOME` if set;
   `%APPDATA%\anytopdf\config.toml` on Windows), or instead the files named by
   `--config PATH` (repeatable) or `ANYTOPDF_CONFIG`; `--no-config` or
   `ANYTOPDF_NO_CONFIG=1` skips files
3. environment variables: `ANYTOPDF_PROFILE=share`, `ANYTOPDF_VIDEO_INTERVAL=2`;
   lists are comma-separated
4. flags: `--profile share`, `--video-interval 2`
5. `--set TABLE.KEY=VALUE` (repeatable), e.g. `--set whisper.model=base`

```toml
# ~/.config/anytopdf/config.toml
profile = "share"
renderer = "pdfa"
plugin_timeout = 120

[video]                    # ANYTOPDF_VIDEO_*, --video-*
interval = 2.0
max_frames = 200

[ocr]                      # ANYTOPDF_OCR_*, --ocr-*
mode = "tesseract"
lang = "eng+deu"

[whisper]                  # a runtime plugin's table, passed to it as `options`
model = "base"
```

Top-level keys are global settings. `[image]`, `[video]`, `[ocr]`, `[captions]`,
`[entities]`, `[colors]`, `[location]`, `[raw]` and `[scan]` are the built-in tables and reject unknown keys; any other table is passed to the
runtime plugin with that manifest name (`-` and `_` match) in the `options` field
of each request. A runtime plugin's keys are set with `--set whisper.model=base`,
or `ANYTOPDF_WHISPER_MODEL` once `[whisper]` is in a config file
(`ANYTOPDF_WHISPER__MODEL` always works). The older flag names (`--ocr`, `--lang`,
`--scene-threshold`, `--dedupe-distance`, `--max-video-frames`,
`--max-image-frames`, `--no-embedded-subtitles`, `--location`, `--date-order`,
`--colors`, `--no-entities`) still work.

`anytopdf config` prints the effective settings with the file, variable or flag
each came from (`--json` emits `anytopdf.config/1`); `anytopdf config --defaults`
prints a starter file; values under keys such as `api_key`, `token` or `password`
are shown as `<redacted>`. The file format is `schemas/config-file.schema.json`. An
invalid value exits 2 and names the key and where it was set. Secrets
(`ANYTOPDF_WEBHOOK_SECRET`, `ANYTOPDF_QUEUE_TOKEN`, `ANYTOPDF_IMAP_*`) and
`ANYTOPDF_PLUGIN_PATH` stay environment-only and are never read as settings, and
no file is read from the current directory, so a folder you convert cannot change
how plugins run.

### Screen capture

`anytopdf capture screen` records the screen with FFmpeg and converts the
recording like any video: a frame every `--interval` seconds plus every scene
change, with near-duplicates dropped, then OCR and the usual pipeline.

```bash
anytopdf capture screen -o session.pdf                  # until Ctrl-C
anytopdf capture screen --duration 600 --interval 10 -o standup.pdf
anytopdf capture screen --display 1 --keep-recording s.mkv -- --ocr tesseract
```

It uses FFmpeg's platform grabber: `avfoundation` on macOS, `gdigrab` (whole
desktop) or `ddagrab` (`--display N`) on Windows, `x11grab` on Linux. Pass
`--input-format` and `--input` for any other FFmpeg input, such as `kmsgrab` on a
Wayland session. macOS needs the Screen Recording permission for the terminal app;
`anytopdf doctor` reports it and the grabber under "Screen capture". Options after
`--` go to `convert`; the recording is deleted unless `--keep-recording` is given,
and the PDF defaults to `screen-<UTC time>.pdf`.

### Progress events

`anytopdf convert --events` writes one NDJSON object per line to stderr
(`anytopdf.events/1`, `schemas/events.schema.json`) instead of human progress and
`error:` lines. Every run ends with exactly one `run.finished` event whose
`status` (`ok`, `partial`, `failed`) and `exit_code` match the process exit code;
a failed run adds an `error` message. A closed stderr pipe never panics.

### PDF/A-3 output

`anytopdf convert` writes tagged PDF/A-3a by default (`--renderer pdfa`);
`--renderer pdf` writes plain PDF through printpdf instead. Pages, page numbering and
the embedded manifest and chunks are the same with both. On top of that, `pdfa`:

- Fonts are always embedded. The first `ANYTOPDF_FONT` entry is the primary font;
  without it the bundled DejaVu Sans (Latin, Greek, Cyrillic, Hebrew, basic Arabic) is
  used, so no system font is needed. `ANYTOPDF_FONT` may list more fonts, separated like `PATH`, and they
  are tried next, followed by common system fonts for Arabic, Hebrew and CJK. A
  fallback font is embedded only when it supplies characters the earlier fonts lack.
  Fonts whose licence forbids embedding are skipped. Characters no font covers are
  left out with a render warning, because PDF/A forbids `.notdef` glyphs.
- Each line is reordered with the Unicode bidi algorithm and shaped, so Arabic and
  Hebrew read correctly. Right-to-left lines carry `ActualText` with the logical
  order for copying and extraction.
- The structure tree has a section per unit, a figure with alternate text per image or
  frame, a paragraph per source line, and an H1 heading on the provenance page.
  Bookmarks point to the first page of each source and to the provenance page. The
  document language is `und` (undetermined).
- The document carries XMP metadata and an sRGB output intent. The manifest and
  chunks are PDF/A-3 associated files (`/AF`, relationship `Data`).
- The hidden layer is written as text with a fill opacity of 0 rather than text
  rendering mode 3, because krilla has no mode 3. It is still searchable and
  extractable.

Output is reproducible under `SOURCE_DATE_EPOCH`. Text the bundled font covers renders
the same on every host; fallback fonts for other scripts come from the host, so such
text can embed different fonts on different hosts.

### Camera RAW and phone photos

```bash
anytopdf convert DSC_0042.NEF IMG_1234.CR3 -o shoot.pdf
anytopdf convert receipt.jpg -o receipt.pdf              # page found, flattened, deskewed
anytopdf convert scans/ --scan-mode off -o as-shot.pdf    # keep photos untouched
```

RAW files use the camera's embedded JPEG preview by default. `--raw-decode develop`
prefers a local developer (`sips`, `dcraw_emu`, `dcraw`, ImageMagick) and falls back to
the preview with an `input.lossy-decode` warning; `--raw-decode preview` never runs a tool.

`--scan-mode auto` (the default) flattens a photo only when it clearly shows a sheet
with text on a darker background, and straightens page-filling scans whose text lines
are skewed; other photos are left as they are. `on` also crops sheets that touch the
frame edges and straightens any photo with text lines. Both options can also be set
with `ANYTOPDF_RAW_DECODE` and `ANYTOPDF_SCAN_MODE`.

### Embedded manifest and chunks

Every converted PDF embeds two JSON attachments: `anytopdf-manifest.json`
(`anytopdf.manifest/1`: sources with SHA-256 and size, units, providers, profile)
and `anytopdf-chunks.json` (`anytopdf.chunks/1`: one chunk per unit with page
traceability, plus an optional `entities` list of `{kind, value}` such as
`{"kind":"url","value":"https://example.com"}` with kinds `url`, `email`, `domain`,
`app`, `date`, `time` and `datetime`; dates and times carry their ISO 8601 value).
Chunks of image pages also carry a `words` list of `{text, x, y, width, height}`
boxes in reading order, from OCR or the source PDF's own text, with the top-left
corner and size given as fractions of the page. Schemas live in `schemas/`. The `share` profile omits absolute
paths. If embedding fails, the same JSON is written beside the PDF as
`<output>.manifest.json` and `<output>.chunks.json` and an informational
`manifest.sidecar` notice is printed.

`anytopdf extract <pdf> --json` prints one `anytopdf.extract/1` document
(`schemas/extract.schema.json`) with `origin` (`embedded` or `sidecar`), the
`manifest`, the `chunks` and `warnings`. A manifest or chunks `schema_version`
other than the supported one adds an `extract.version-mismatch` warning (also
on stderr) but still exits 0. A version-matched manifest or chunks file that
does not match its schema exits 3 (input); the error names the document and the
first failing JSON path. A PDF with no embedded or sidecar manifest exits
3 (input).

### Search across files

`anytopdf convert … --index` also records the output in a local SQLite search
index (FTS5, bundled; no server). `anytopdf search` then searches every indexed
PDF at once and prints the PDF, page, time in the source, kind, the matching text
and the source file:

```bash
anytopdf convert meeting.mp4 -o meeting.pdf --index --collection work
anytopdf index add old-archive/*.pdf       # PDFs made earlier, from their embedded chunks
anytopdf search budget review              # every word must match
anytopdf search '"red car"' --kind object  # a phrase, only object detections
anytopdf search --person Alice --json      # faces recognised as Alice
anytopdf index list
anytopdf index remove old-archive/a.pdf
```

The index lives at `ANYTOPDF_INDEX`, or `index.sqlite` in the per-user data
directory (`~/.local/share/anytopdf` on Linux, `~/Library/Application
Support/anytopdf` on macOS, `%LOCALAPPDATA%\anytopdf` on Windows); `--index-db`
names another. It is never written into a PDF. `convert --index` records one
`chunk` entry per unit (the unit's searchable text, as in `anytopdf-chunks.json`)
and one entry per annotation with its kind (`ocr`, `caption`, `transcript`,
`face`, `object`, `scene`, `location`, …), provider, confidence, region, time
range and attributes, so `--kind` and `--person` (faces whose `person` attribute
names someone) can narrow a search. It records what the PDF holds, after the
output profile is applied. `index add` reads existing PDFs back from their
embedded chunks, so they only have `chunk` entries; an unchanged PDF that
`convert --index` already recorded keeps its richer record. A unit's `chunk`
entry is left out of results when one of its annotations matched on its own,
because the annotation carries the region and time. `--collection NAME` tags
PDFs (on `convert` and `index add`) and filters searches. Re-indexing a PDF
replaces its entries.

Words are matched case- and accent-insensitively; `"quoted words"` match as a
phrase and a trailing `*` matches a prefix. `search --json` prints one
`anytopdf.search/1` document (`schemas/search.schema.json`) and `index add|list
--json` one `anytopdf.index/1` document. Searching without an index exits 3. The
index has room for per-unit embeddings for semantic search.
### Asking questions

`anytopdf ask <pdf-or-directory> "<question>"` answers a question from PDFs that
anytopdf produced. It reads each PDF's embedded chunks (every `*.pdf` directly in
a directory), ranks them with BM25 and keeps the best `--top N` (default 8) as
numbered passages. Each passage cites its PDF, pages, time range for audio and
video, and the original file name.

With no LLM configured, ask prints those passages. To get a written answer that
cites them as `[n]`, point it at any OpenAI-compatible server such as llama.cpp,
Ollama, vLLM or LM Studio:

```bash
export ANYTOPDF_LLM_URL=http://127.0.0.1:11434/v1   # POSTs to $URL/chat/completions
export ANYTOPDF_LLM_MODEL=llama3.2                  # optional: ANYTOPDF_LLM_API_KEY, ANYTOPDF_LLM_TIMEOUT
anytopdf ask meeting.pdf "What did we decide about the launch date?"
```

The question and the retrieved passages are sent to that URL, so use a local
server for private files. If the endpoint fails, ask prints an `ask.llm-failed`
warning and returns the passages, still exiting 0. `--no-llm` skips the endpoint.
`--json` prints one `anytopdf.ask/1` document (`schemas/ask.schema.json`):
`mode` (`llm` or `retrieval`), `answer`, `cited` passage numbers, `passages` and
`warnings`. A missing or unreadable PDF exits 3, an empty question or a
non-HTTP `ANYTOPDF_LLM_URL` exits 2.

### Watching a mailbox (IMAP)

`anytopdf watch imap` turns each new message in one mailbox into its own PDF
(release builds include it; source builds need the default `imap` feature):

```bash
export ANYTOPDF_IMAP_PASSWORD=...   # or --password-file; never a command-line flag
anytopdf watch imap --host imap.example.com --user scans@example.com \
  --allow-from example.com --output-dir ~/mail-pdfs -- --ocr auto --profile share
```

- Each message is fetched with `BODY.PEEK[]` (the mailbox is not modified unless
  `--mark-seen` or `--move-to <mailbox>` is given), saved as a raw `.eml`, and
  converted by a child `anytopdf convert` into
  `<output-dir>/<mailbox>-<uidvalidity>-<uid>.pdf`. Arguments after `--` go to
  convert. The child does not inherit `ANYTOPDF_IMAP_PASSWORD` or
  `ANYTOPDF_IMAP_OAUTH_TOKEN`. The built-in email importer renders each message
  (headers, body and attachments through the normal importers).
- `--queue <QUEUE>` instead of `--output-dir` hands each message to an
  `anytopdf queue` directory as a job (origin `imap`) carrying the options after
  `--`; run `anytopdf queue work <QUEUE>` to convert, with webhooks if wanted.
- `--allow-from` (repeatable) accepts an address (`scanner@example.com`) or a whole
  domain (`example.com`, no subdomains); mail from anyone else is skipped, left in
  the mailbox and not retried. The From header is easy to forge, so add
  `--require-dmarc <authserv-id>` (for example `mx.google.com` or `outlook.com`) to
  also require `dmarc=pass` in the `Authentication-Results` header your own mail
  server added; copies of that header further down the message are ignored.
- `--auth login` (default) sends IMAP `LOGIN` with a password or app password.
  `--auth xoauth2` signs in to Gmail or Microsoft 365 with an OAuth2 access token
  from `ANYTOPDF_IMAP_OAUTH_TOKEN` or `--oauth-token-file`. The file is read again
  on every connection, so a separate token refresher (for example a cron job using
  your OAuth client's refresh token) can replace it; anytopdf does not run a
  browser sign-in.
- Progress lives in `<state-dir>/state.json` (default `.anytopdf-imap` inside the
  output or queue directory), keyed by the mailbox's UIDVALIDITY. The first run
  only picks up mail that arrives afterwards; `--backfill` takes existing mail
  too. Failed conversions are retried on later checks up to `--max-attempts`, then
  the message is kept in `<state-dir>/failed`. Messages over `--max-message-bytes`
  are skipped. Delivery is at least once: a crash mid-conversion converts that
  message again.
- `--tls implicit` (default, port 993) or `--tls starttls` (port 143) use the
  operating system's trusted roots plus an optional `--ca-file`; `--tls none` is
  refused unless the host is loopback.
- The watcher waits with IMAP IDLE when the server supports it and otherwise polls
  every `--poll-interval` seconds; it reconnects with backoff after network errors.
  `--once` checks a single time and exits (for cron). Connection settings may also
  come from `ANYTOPDF_IMAP_HOST`, `_PORT`, `_TLS`, `_USER`, `_MAILBOX`, `_AUTH`,
  `_PASSWORD_FILE` and `_OAUTH_TOKEN_FILE`. Use one `--state-dir` per mailbox and
  one watcher per state directory.

### Job queue and webhooks

`anytopdf queue` runs conversions from a plain queue directory, so it needs no
database or daemon. Only `queue serve` listens on a port; the worker only makes outbound
requests when `--webhook` is given.

```bash
anytopdf queue add ~/scans-queue invoice.jpg -- --profile share
anytopdf queue work ~/scans-queue -- --ocr auto     # options for inbox files
anytopdf queue work ~/scans-queue --once            # drain, then exit
anytopdf queue status ~/scans-queue
```

- Files dropped into `QUEUE/inbox/` are claimed once two scans
  (`--poll-interval`, default 2 seconds) see the same size and mtime. Dotfiles and
  `*.part`, `*.tmp`, `*.crdownload`, `*.download` and `*.partial` names are ignored.
- Job records (`anytopdf.job/1`, `schemas/job.schema.json`) move between
  `QUEUE/jobs/pending`, `running`, `done` and `failed` by atomic rename, so several
  workers can share a queue. PDFs land in `QUEUE/outbox/`.
- Each job runs `anytopdf convert --events --json` as a child process;
  `QUEUE/work/<id>/events.ndjson` keeps its NDJSON events and `convert.json` its
  result. Convert options after `--` are checked by the convert parser; `-o`,
  `--output-dir`, `--events`, `--json` and `--dump-graph` belong to the worker.
- `--job-timeout` (default 3600 seconds) stops an overrunning conversion. A
  running job whose worker died is requeued once its lease (timeout plus 60
  seconds) expires.
- Runtime plugins in queued jobs run under `--plugin-sandbox contain` unless you
  pass another level (`anytopdf --plugin-sandbox strict queue work QUEUE`, or `off`
  to opt out), so no process a plugin starts outlives its call.

`--webhook URL` (repeatable) sends [Standard Webhooks](https://www.standardwebhooks.com)
`job.received`, `job.completed` and `job.failed` events (`anytopdf.webhook/1`,
`schemas/webhook.schema.json`). Requests carry `webhook-id`, `webhook-timestamp`
and a `webhook-signature` HMAC-SHA256 over `id.timestamp.body`, keyed by
`ANYTOPDF_WEBHOOK_SECRET` (create one with `anytopdf queue secret`). Payloads
carry file names and queue-relative output paths, never absolute paths or error
text. Deliveries are stored in `QUEUE/webhooks/pending/` before they are sent and
are retried after 5 s, 5 min, 30 min, 2 h, 5 h, 10 h and 10 h; a 410 response or
the last failure moves them to `QUEUE/webhooks/failed/`. Delivery is
at-least-once, so receivers should deduplicate on `webhook-id`.

`anytopdf queue serve QUEUE` is the opt-in HTTP upload intake. It listens on
`127.0.0.1:8640` by default; any other address needs `--tls-cert` and `--tls-key`,
and `0.0.0.0` or `::` also needs `--allow-public-bind`. Every request needs
`Authorization: Bearer $ANYTOPDF_QUEUE_TOKEN` (at least 16 characters; `anytopdf
queue secret` makes a good one). Uploads are capped by `--max-upload-mb` (default
100) and must send `Content-Length`. Run `queue work` alongside it to convert them.

```bash
export ANYTOPDF_QUEUE_TOKEN=$(anytopdf queue secret)
anytopdf queue serve ~/scans-queue -- --profile share   # options for uploaded files
curl -H "Authorization: Bearer $ANYTOPDF_QUEUE_TOKEN" \
  --data-binary @scan.jpg 'http://127.0.0.1:8640/v1/jobs?filename=scan.jpg'
```

| Request | Answer |
| --- | --- |
| `POST /v1/jobs?filename=NAME` with the file as the body | `202` and the job (`job_id`, `state`, `origin`, `inputs`) |
| `GET /v1/jobs/<job_id>` | `200` and the job, with `output`, `status`, `exit_code` and `pages` once finished |
| `GET /v1/jobs/<job_id>/output` | `200` and the PDF, or `409` until the job has succeeded |
| `GET /v1/search?q=WORDS&kind=K&person=P&collection=C&limit=N` | with `--search`: `200` and an `anytopdf.search/1` document (without the index path); `404` otherwise |

Uploaded names are reduced to a plain file name inside the job's work directory.
Clients cannot pass convert options; the server's options after `--` apply.
Errors are JSON `{"error": "..."}` with `401`, `411`, `413`, `404` or `405`.
`--search` (with `--index-db`, default as for `anytopdf search`) opens the search
endpoint; have the worker record conversions with `queue work QUEUE -- --index`.

### Remote printing

The print helper listens on localhost only. `anytopdf print remote` lets phones and
laptops on your Tailscale or WireGuard network print to it: it accepts TLS
connections (`ipps://`), admits only allowlisted peers, asks for a print user's
password, then passes the job to the helper.

```bash
tailscale cert printer.tailnet-name.ts.net
echo 'a long password' | anytopdf print passwd adeel --users ~/.anytopdf/print-users.json
anytopdf print remote --listen 100.101.102.103:8631 --allow-tailnet \
  --tls-cert printer.tailnet-name.ts.net.crt --tls-key printer.tailnet-name.ts.net.key \
  --users ~/.anytopdf/print-users.json
```

It refuses a non-loopback listener without users and an allowlist, and
`0.0.0.0`, `::` or a `/0` allowlist without `--allow-public-bind`. The signed-in
user replaces the IPP `requesting-user-name`, so the helper records who really
printed; `--receipts receipts.jsonl` also appends one `anytopdf.print-receipt/1`
line per job with the time, peer address, user and job name. The PDF of a print
job carries the job id, name, user and format as `print.*` source metadata in the
manifest and as a receipt on the provenance page (`archive` profile only; `share`
drops them). `anytopdf doctor` reports the print helper and Tailscale. Discovery:
`anytopdf print advertise` announces the printer over multicast DNS on the local
network (IPP Everywhere `_ipps._tcp` with the AirPrint `_universal` subtype);
multicast does not cross a VPN, so `anytopdf print dns-sd --domain home.example
--host printer.home.example` prints unicast DNS-SD records to add to your own DNS
for remote Apple clients, and `anytopdf print url` prints the `ipps://` URL to add
the printer by hand on Windows and Android. See `docs/design/remote-printing.md`.

### MCP server

`anytopdf mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io)
server over stdio (newline-delimited JSON-RPC 2.0) so agents can call anytopdf
as tools:

| Tool | Runs | Returns |
| --- | --- | --- |
| `convert` | `anytopdf convert --json` | `anytopdf.convert/1` report |
| `extract` | `anytopdf extract --json` | `anytopdf.extract/1` document |
| `ask` | `anytopdf ask --json` | `anytopdf.ask/1` answer and cited passages |
| `probe` | `anytopdf probe --json` | `anytopdf.probe/1` document |
| `search` | `anytopdf search --json` | `anytopdf.search/1` document |
| `capabilities` | `anytopdf capabilities --json` | `anytopdf.capabilities/1` document |

Each call re-runs the same executable, so tools keep the CLI's validation,
overwrite protection, profiles and exit codes. The JSON document is returned as
both text and `structuredContent`; a non-zero exit becomes a tool result with
`isError: true` whose text starts with the exit code and class, followed by
stderr. Global flags given before `mcp` (`--no-plugins`, `--plugin-timeout`,
`--allow-plugin-kind`, `--deny-plugin-kind`) apply to every call. Paths are local
to the server and relative paths resolve against its working directory, so give
agents absolute paths. The server reads and writes files with your permissions,
exactly like the CLI.

Register it with an MCP client, for example Claude Code:

```bash
claude mcp add anytopdf -- anytopdf --no-plugins mcp
```

or in a client's JSON configuration:

```json
{"mcpServers": {"anytopdf": {"command": "anytopdf", "args": ["mcp"]}}}
```

## Roadmap

anytopdf is meant to produce an evidence file: one PDF that is both the human
rendition and the machine index (embedded manifest, chunks, provenance and hashes),
works offline, and is ready for agents to read. `- [x]` is implemented on `main`
and `- [ ]` is planned.
Per-release detail is in [ROADMAP.md](ROADMAP.md).

### Searchable text and diagnostics
- [x] Content-only hidden text layer (no paths or metadata, no per-page duplication)
- [x] Typed diagnostics with stable codes and INFO/WARNING severities
- [x] `--strict` ignores missing optional providers
- [x] Provider version detection
- [x] Every frame of multi-frame TIFF/GIF becomes a page (`--max-image-frames` caps it, warning `input.frames-not-imported` when frames are dropped)
- [x] Content-sniffed text importer (csv, json, log, code; lossy for non-UTF-8)
- [x] `--transcript` is never silently ignored

### CLI and automation
- [x] Simple form `anytopdf <inputs...> -o out.pdf`, no subcommand, `-o` anywhere
- [x] Automatic `<stem>.pdf` naming, numbered and never clobbering
- [x] `--output-dir` writes one PDF per input
- [x] Distinct exit codes
- [x] Batch continues past failed inputs by default, `--fail-fast` to stop
- [x] `--json` for convert, probe, doctor and plugins, with capabilities and published JSON Schemas
- [x] Help text on every flag
- [x] NDJSON progress events
- [x] `anytopdf ask` answers questions with cited passages (MCP `ask` tool)
- [x] One definition per option: TOML config file, `ANYTOPDF_*` variable and flag (`anytopdf config` shows where each value came from)
- [x] `anytopdf setup whisper` turns on speech to text in one step

### Evidence file and provenance
- [x] Content-derived source and unit IDs with SHA-256 and size
- [x] Source anchors (time span, bounding box, byte range) and a page map
- [x] Embedded versioned manifest and chunks (or sidecar), plus `anytopdf extract --json`
- [x] Byte-reproducible output with `SOURCE_DATE_EPOCH` and recorded provider versions
- [x] Archive and share privacy profiles
- [x] Provenance page as the last page (`--no-provenance-page` to omit)
- [ ] Deterministic chunk IDs and semantic page/chunk headings
- [ ] Provenance graph export
- [ ] Incremental index mode
- [x] Cross-file search index: `convert --index`, `index add`, `search` (also over MCP and `queue serve --search`)
- [x] PDF/A-3a output (`--renderer pdfa`)
- [x] Tagged PDF and bookmarks (`--renderer pdfa`)

### Rendering
- [x] Spike: layout and writer options
- [x] krilla 0.8 writer behind `--renderer pdfa` (Rust 1.92, invisible text via fill opacity)
- [x] `pdfa` as the default renderer
- [x] Labelled boxes for object, face and OCR regions on visual pages (`--draw-boxes`)
- [ ] Rendered Markdown
- [x] Arabic, Hebrew and CJK shaping, bidi and font fallback (`--renderer pdfa`)

### Input formats
- [x] PDF input: Poppler renders each page and `pdftotext -bbox-layout` lines become the hidden text layer, so only textless pages are OCR'd; without Poppler the page text is imported as text pages with a `provider.missing` notice
- [x] HTML importer: `.html`/`.htm`/`.xhtml` or a doctype becomes a text page without scripts, styles or markup
- [x] URL inputs: web pages become readable text plus a headless-Chrome snapshot, video and podcast links go through yt-dlp with their captions, other links are imported by content type
- [x] Email importer: `.eml` and `.mbox` messages become text pages; attachments and forwarded messages are imported through the registry (nested at most 4 deep), unimportable ones warn `input.members-not-imported`
- [x] Chat exports: WhatsApp, Telegram, Slack and iMessage (`imessage-exporter` text) conversations with speakers, timestamps and attachments inline
- [x] Archive importer: zip and (gzipped) tar members are extracted into the job workspace under sanitized names (no traversal, links skipped) with caps of 512 MiB per member, 1 GiB per archive, 10,000 entries, a 200:1 zip compression ratio, and 2 GiB / 10,000 members per input across nesting
- [x] HEIC/HEIF/AVIF importer: the first of `sips`, `heif-convert`, `magick` or `convert` that decodes the photo produces the page; without one the input is skipped with `import.failed`
- [x] Camera RAW importer: largest embedded JPEG preview (lossless sensor streams skipped, container orientation applied), developed by `sips`, `dcraw_emu`, `dcraw` or ImageMagick when the preview is missing or small
- [x] Phone-photo scans: page detection, perspective correction and deskew before OCR
- [x] Office documents through LibreOffice and Poppler
- [x] JSON and JSON Lines: one searchable chunk per record
- [x] Link lists and browser bookmark exports (`--links`), with bookmark folders as PDF bookmarks
- [ ] CAD, image stacks, IGL plugin and a generic command-adapter plugin

### Media enrichment
- [x] Whisper transcription as a runtime plugin
- [x] Location: offline reverse geocoding of GPS fixes and place names in text
- [x] Face presence, count, bounds and landmarks as a runtime plugin
- [x] Audio events: speech, music, silence, raised voices, and laughter, applause and other sounds with an AudioSet model
- [x] Face recognition against a local, user-enrolled face index (`--recognize-faces`)
- [x] Object detection (`anytopdf-plugin-objects`, YOLO) and zero-shot scene tags with CLIP embeddings (`anytopdf-plugin-clip`)
- [x] Keyframe captions, activities and video and scene summaries from a local vision model (`anytopdf-plugin-vlm`)
- [x] Sentiment and tone of transcript, caption, OCR and document text (`anytopdf-plugin-sentiment`)
- [x] Dominant colours per image and keyframe
- [ ] Barcode and QR extraction
- [ ] Audio chapters and speaker turns
- [ ] OCR-text-aware video frame retention

### Intake channels
- [x] Webhooks (Standard Webhooks: job.received, job.completed, job.failed, HMAC signature, retries)
- [x] Shared job queue with watched folder input
- [x] HTTP upload input for the job queue
- [x] IMAP watcher: IDLE and polling, sender allowlist with DMARC check, OAuth2 (XOAUTH2) tokens, job-queue hand-off
- [ ] IMAP rules beyond sender and search criteria (Paperless-ngx style), quarantine folder
- [ ] Email-to-print
- [x] Screen capture (`anytopdf capture screen`)

### Printing
- [x] Spike: PAPPL printer feasibility
- [x] Network printer via IPP Everywhere, built on PAPPL as an optional helper process ([`helpers/anytopdf-printer`](helpers/anytopdf-printer/README.md)); PWG Raster and Apple Raster print jobs keep their paper size
- [ ] AirPrint and Mopria certification
- [x] IPP over TLS with a password, localhost by default
- [x] Print receipts on the provenance page
- [x] Remote printing over Tailscale or WireGuard with DNS-based discovery
- [ ] Microsoft Universal Print investigation

### Security and plugins
- [ ] Untrusted-input handling shipped with the intake channels: sandboxed conversion without network, size and page caps, zip-bomb rejection, per-sender budgets
- [x] Opt-in OS sandbox (`--plugin-sandbox strict`, Linux and macOS) and descendant process containment (`contain`, all platforms) for runtime plugins
- [ ] Hard CPU, memory and disk quotas for runtime plugins; sandboxing for built-in providers; Windows `strict`

### Builds and distribution
- [x] Release build with LTO and strip (8.58 MB to 6.25 MB on macOS arm64)
- [x] Spike: slim and full build shapes
- [ ] Slim and full builds (full bundles LGPL decode-only ffmpeg, OCR models, Whisper base, Noto fonts)
- [x] Homebrew formula, Scoop manifest, cargo-binstall metadata and a GHCR container image built from the release archives
- [x] Published Homebrew tap and Scoop bucket (`Formula/` and `bucket/` in this repository)
- [x] `curl | sh` installer (`install.sh`, checksum-verified)
- [ ] winget and npx/uvx wrappers
- [ ] Signing and notarization
- [x] MCP server mode (`anytopdf mcp`)
- [ ] Agent skill and `llms.txt`

## Dependencies

Building from source:
- GNU Make and Bash to start the bootstrap (Git Bash on Windows).
- Python 3.11+ for build verification and release tooling.
- Rust 1.92.0 (pinned in `rust-toolchain.toml`); packaged binaries do not require Rust.
- `Cargo.lock` pins dependencies compatible with this toolchain.

Optional runtime providers:
- `ffmpeg` / `ffprobe`: video/audio demuxing, keyframes and screen capture
- `exiftool`: rich metadata
- `tesseract`: OCR fallback
- Python + `doctr`: docTR OCR fallback

On macOS the `apple-vision` Cargo feature uses native Vision directly from Rust.

## Build

```bash
make
./target/release/anytopdf doctor
# Optional: install media/OCR/PDF inspection providers separately
make providers
```

Default `make` installs missing supported build tools, the pinned Rust toolchain,
rustfmt and Clippy, then builds the optimized CLI. Bootstrap uses Homebrew on
macOS, apt/dnf/pacman on Linux, or Chocolatey from Git Bash on Windows. Package
installation may need administrator access and network access. GNU Make and Bash
must already be available to start it. On macOS, finish the Apple Command Line
Tools installer if prompted and rerun. Windows needs Visual Studio C++ Build
Tools for MSVC; bootstrap does not install that compiler or Git Bash.

`make deps` prepares build tools only. `make providers` additionally installs
FFmpeg, ExifTool, Tesseract and Poppler (plus DejaVu fonts on Linux); it does not
install docTR models or perform audio transcription. Missing providers remain
optional for ordinary conversions. `make doctor` reports actual availability.

Python selection honors `PYTHON=/absolute/path/to/python` (quote paths containing
spaces), otherwise tries Python 3.11+ candidates. An invalid explicit override
fails instead of silently choosing another interpreter. Once tools are ready,
`cargo build --release --locked` remains available directly.

Linux fully-static (run on Linux with `musl-tools` installed):

```bash
rustup target add x86_64-unknown-linux-musl
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
  cargo build --release --locked -p anytopdf --no-default-features --target x86_64-unknown-linux-musl
```

Windows MSVC builds use static CRT flags from `.cargo/config.toml`.

macOS cannot fully statically link Apple system frameworks; the application
binary itself remains a single executable and uses the system Vision framework.

## Design invariants

1. Core orchestration never switches on individual file extensions.
2. Importers own format knowledge.
3. Enrichers append annotations; they do not rewrite unrelated data.
4. Every annotation records provider/provenance.
5. Temporary derived artifacts live inside one job workspace.
6. Renderer consumes only the normalized graph, never format-specific objects.
7. Plugin failures are isolated and reported; one failed optional enricher does
   not invalidate the entire job.
8. Runtime plugins use paths/JSON, not Rust ABI structs.
9. The PDF is the canonical portable artifact; the graph can also be serialized
   as JSON for debugging or future indexing.

## Reliability and operation

Running without a subcommand shows help. `probe FILE` reports the selected importer
without decoding media, extracting frames, or running enrichers.

`convert`, `probe`, `doctor` and `plugins` accept `--json` and then write exactly one versioned JSON
document to stdout (diagnostics go to stderr); contracts live in `schemas/`.
`convert --json` always emits one `anytopdf.convert/1` document, including on failure
(`status` is `ok`, `partial` when inputs were skipped but exit is 0, or `failed`; `exit_code`
mirrors the process exit code). With `--profile share`, paths in it are base names.
`capabilities` prints a table of what this binary and environment support: built-in
importers, enrichers and renderers, OCR providers, external tools and runtime plugins, each
marked available, partial or missing, followed by how to enable what is missing and how to add a
plugin (`capabilities --help` explains the legend). `capabilities --json` lists exit codes,
diagnostic codes, profiles, OCR modes, importers and schema ids (`anytopdf.capabilities/1`).

```bash
anytopdf --no-plugins convert notes.txt --ocr off -o notes.pdf
anytopdf convert notes.txt -o notes.pdf --overwrite
anytopdf convert recordings/ --strict -o ../archive.pdf --dump-graph ../archive.json
anytopdf --plugin-timeout 30 --allow-plugin-kind importer plugins
anytopdf --deny-plugin-kind renderer convert document.example -o result.pdf
```

Without `-o`, output is `<input-stem>.pdf` for one input (else `anytopdf.pdf`) in the
current directory, numbered (`notes-1.pdf`) if that name exists; `--overwrite` reuses the
unnumbered name. An explicit existing `-o` requires `--overwrite`; input files and explicit transcripts are
protected even with that flag. Put outputs outside input directories so subsequent
directory scans do not ingest them. Writes are staged and atomically published.

### --output-dir

`anytopdf a.txt b.png --output-dir out/` writes one PDF per input instead of one merged
PDF. Each PDF has its own provenance page and embedded manifest listing only that source.
Names are `<input-stem>.pdf` in input order; a name already used in the run, by an existing
file (without `--overwrite`) or by an input becomes `<stem>-N.pdf`. All PDFs are rendered
before any is published. `--dump-graph` still writes one whole-run graph. It conflicts with
`-o` (exit 2).
PDF and JSON outputs are separate file transactions. `--strict` refuses to publish
when ingestion or rendering produces warnings; normal mode reports warnings and
keeps usable content. `--quiet` suppresses the success summary, not warnings.

### Exit codes

| Code | Class | Meaning |
|---|---|---|
| 0 | success | The PDF was published. |
| 1 | internal | Unexpected failure. |
| 2 | usage | Invalid option or value. |
| 3 | input | Missing, unreadable or unusable input. |
| 4 | provider | An explicitly requested provider (for example `--ocr tesseract`) is unavailable. |
| 5 | strict | `--strict` stopped on warnings before publishing. |
| 6 | render | Rendering failed; nothing was published. |
| 7 | fail-fast | `--fail-fast` stopped on a skipped input; nothing was published. |

Batch behaviour (request Amendment 1): by default a failing input (corrupt,
unsupported or unreadable) is skipped with a warning naming the file, the rest of
the batch continues, and the run exits 0 if a PDF was published. stderr ends with
`Summary: N converted, M skipped` and one `skipped <input>: [<code>] <reason>` line
per skipped input. `--fail-fast` aborts without publishing and exits 7; `--strict`
still exits 5 on any warning; if no input is usable the run exits 3.

### Diagnostics and strict mode

Each notice prints to stderr as `INFO [code]: message` or `WARNING [code]: message`.
Informational codes (`provider.missing`, `ocr.fallback`, `manifest.sidecar`) report
optional capabilities or fallbacks and never fail `--strict`. Every other code is a
warning (for example `input.unsupported`, `import.failed`, `provider.failed`,
`render.warning`) and makes `--strict` stop before publishing.

Plugin invocations default to a 60-second timeout. Captured stdout, stderr and
plugin response JSON each have a 16 MiB limit. Metadata providers have 30-second
timeouts, OCR subprocesses 180 seconds, subtitle extraction 120 seconds and each
video extraction pass 300 seconds. `--max-video-frames` also bounds extracted frames
per pass; `0` means no frame-count limit. These limits are safeguards, not an OS
sandbox: by default plugins run with your account's permissions. Install only
trusted plugins or use `--no-plugins`. `--plugin-sandbox contain` ends every process
a plugin starts with its call; `--plugin-sandbox strict` (Linux and macOS) also
limits plugin writes to the job workspace and blocks network access. See
`PLUGIN_PROTOCOL.md` for policy details.

Images are decoded by content, normalized to PNG in the temporary workspace, and
rotated according to EXIF orientation. OCR coordinates refer to that normalized
image. Every frame of a GIF or multi-page TIFF becomes a page carrying a `frame` anchor; `--max-image-frames N` caps the count (0 = unlimited). Frames are not deduplicated and each is OCRed.

Fonts are subset to the required glyphs and embedded in the PDF. The default `pdfa`
renderer uses its bundled DejaVu Sans and falls back to system fonts for other scripts;
`--renderer pdf` loads a system font. Set `ANYTOPDF_FONT` to a TTF file (or several,
separated like `PATH`) for a particular script. Missing glyphs produce warnings.
Shaping, bidirectional layout and font fallback apply to `pdfa` only; the printpdf
renderer draws characters in one font, left to right.
Markdown is rendered as plain text. Audio requires sidecar/explicit transcripts or
a plugin for speech recognition; docTR may download model weights on first use.

`--dump-graph` is a diagnostic sidecar, not a portable media bundle: visual paths
into the temporary workspace are removed. `--profile archive|share` (default
`archive`) selects metadata detail; `share` also strips local paths, including from stderr diagnostics, the Summary and `--json` messages.
`--draw-boxes[=objects,faces,ocr|all]` draws labelled boxes for detected regions over
image and video-frame pages (bare `--draw-boxes` draws objects and faces); the source
images are not modified.
`--no-provenance-page` omits the provenance page. `SOURCE_DATE_EPOCH` fixes the
creation time for reproducible output; an invalid value exits 2.

## Development and release checks

Use GNU Make, Bash, the pinned Rust toolchain, and Python 3.11+:

```bash
make help
make verify
make ci SMOKE_FLAGS=--require-poppler
make package
```

`make ci` runs formatting, checks, Clippy, Rust tests in both feature configurations,
Python tests, a release build and PDF smoke checks. `make package` builds and
smoke-tests the CLI, then writes its archive and SHA-256 checksum to `dist/`.
Individual targets include `build`, `build-release`, `fmt`, `check`, `lint`, `test`,
`test-no-default`, `test-python` and `doctor`. `make clean` keeps release archives.
**`make release` publishes to GitHub**: it versions, verifies, commits, tags,
pushes and waits for publication. Use `make build-release` for a local binary,
`make release-plan` for a version/notes preview, and `make commit-check` to check
Conventional Commits. See [release and recovery procedures](RELEASING.md).

Select a build target with `TARGET=<triple>`; Linux musl builds also use
`NO_DEFAULT_FEATURES=1`. Set `PYTHON=python` on Windows (Git Bash and GNU Make are
required), `CARGO_TARGET_DIR=<path>` for a separate build cache, or `DIST_DIR=<path>`
for a separate archive directory. Verification runs on the host; cross-compiled
smoke/package targets need an executable that can run on that host.

The smoke test verifies PDF structure, graph output and overwrite protection. With
Poppler (`pdftotext`, `pdfinfo`), it also checks extracted Unicode text and pagination.
`SMOKE_FLAGS=--require-poppler` requires those tools; add `--strict` when ExifTool
and a Unicode-capable font are installed.

GitHub CI runs `make ci` on Linux, macOS and Windows for branch pushes and pull
requests. Full-history push checks enforce Conventional Commits; PR checks enforce
the title for squash merging. Dispatch/reusable calls without push/PR context do
not assume event fields. Pushing a `v*` tag matching the Cargo version and lockfile
runs the checks, packages five native targets, verifies the exact archive/checksum
inventory and publishes a GitHub Release using that changelog section. Manual
release runs upload workflow artifacts only. See `RELEASING.md` for the release
procedure and validation evidence. All builds use direct shell commands.

## Contributing

Bug reports, importers and plugins are welcome. [CONTRIBUTING.md](CONTRIBUTING.md)
covers the local checks and commit conventions, and
[PLUGIN_PROTOCOL.md](PLUGIN_PROTOCOL.md) is the place to start for a new format.
Report security issues privately as described in [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. The bundled DejaVu Sans font ships
under its own [license](crates/anytopdf-pdf/fonts/LICENSE-DejaVu.txt). The embedded
gazetteer (`crates/anytopdf-builtin/data/geonames-cities1000.tsv.gz`) contains data
from [GeoNames](https://www.geonames.org/), licensed
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/); location annotations
name it in `attributes.gazetteer`.
