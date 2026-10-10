# syntax=docker/dockerfile:1
#
# anytopdf with FFmpeg, ExifTool, Tesseract and a Unicode font.
#
#   docker build -t anytopdf .
#   docker run --rm -v "$PWD:/work" anytopdf notes.txt photo.jpg -o out.pdf
#   docker run --rm -i -v "$PWD:/work" anytopdf mcp
#
# The release workflow passes BINARY=prebuilt and copies the musl release
# binaries to dist/docker/<arch>/, so the image ships the exact binaries that
# were smoke-tested and checksummed instead of rebuilding them.
#
# The workspace's runtime plugins (audio-events, clip, face-id, faces, objects,
# sentiment, vlm and whisper) are installed in /opt/anytopdf/plugins but stay
# off unless ANYTOPDF_PLUGIN_PATH names that folder. --build-arg WHISPER=cpp
# also compiles whisper.cpp's whisper-cli and turns the Whisper plugin on;
# mount a ggml model at /models/ggml-base.en.bin (or set ANYTOPDF_WHISPER_MODEL):
#
#   docker build --build-arg WHISPER=cpp -t anytopdf:whisper .
#   docker run --rm -v "$PWD:/work" -v "$HOME/models:/models:ro" anytopdf:whisper talk.mp3 -o talk.pdf

ARG BINARY=source
ARG WHISPER=none

FROM rust:1.92-bookworm AS source
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p anytopdf -p anytopdf-plugin-audio-events -p anytopdf-plugin-clip -p anytopdf-plugin-face-id -p anytopdf-plugin-faces -p anytopdf-plugin-objects -p anytopdf-plugin-sentiment -p anytopdf-plugin-tika -p anytopdf-plugin-vlm -p anytopdf-plugin-whisper --no-default-features --features anytopdf/imap \
    && install -m 0755 target/release/anytopdf target/release/anytopdf-plugin-audio-events target/release/anytopdf-plugin-clip target/release/anytopdf-plugin-face-id target/release/anytopdf-plugin-faces target/release/anytopdf-plugin-objects target/release/anytopdf-plugin-sentiment target/release/anytopdf-plugin-tika target/release/anytopdf-plugin-vlm target/release/anytopdf-plugin-whisper /

FROM scratch AS prebuilt
ARG TARGETARCH
COPY dist/docker/${TARGETARCH}/anytopdf dist/docker/${TARGETARCH}/anytopdf-plugin-audio-events dist/docker/${TARGETARCH}/anytopdf-plugin-clip dist/docker/${TARGETARCH}/anytopdf-plugin-face-id dist/docker/${TARGETARCH}/anytopdf-plugin-faces dist/docker/${TARGETARCH}/anytopdf-plugin-objects dist/docker/${TARGETARCH}/anytopdf-plugin-sentiment dist/docker/${TARGETARCH}/anytopdf-plugin-tika dist/docker/${TARGETARCH}/anytopdf-plugin-vlm dist/docker/${TARGETARCH}/anytopdf-plugin-whisper /

FROM ${BINARY} AS binary

FROM debian:bookworm-slim AS whisper-none
RUN mkdir -p /out/bin

FROM debian:bookworm-slim AS whisper-cpp
ARG WHISPER_CPP_REF=v1.9.4
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential ca-certificates cmake git \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${WHISPER_CPP_REF}" https://github.com/ggml-org/whisper.cpp /whisper \
    && cmake -S /whisper -B /whisper/build -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
        -DGGML_NATIVE=OFF -DGGML_OPENMP=OFF -DWHISPER_BUILD_TESTS=OFF \
        -DCMAKE_EXE_LINKER_FLAGS="-static-libstdc++ -static-libgcc" \
    && cmake --build /whisper/build --target whisper-cli -j"$(nproc)" \
    && install -D -m 0755 /whisper/build/bin/whisper-cli /out/bin/whisper-cli
# Putting the plugin on PATH next to its engine is what turns it on.
COPY --from=binary /anytopdf-plugin-whisper /out/bin/

FROM whisper-${WHISPER} AS whisper

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates ffmpeg libimage-exiftool-perl tesseract-ocr tesseract-ocr-eng fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 anytopdf
COPY --from=binary --chmod=0755 /anytopdf /usr/local/bin/anytopdf
COPY --from=binary --chmod=0755 /anytopdf-plugin-audio-events /opt/anytopdf/plugins/anytopdf-plugin-audio-events
COPY --from=binary --chmod=0755 /anytopdf-plugin-clip /opt/anytopdf/plugins/anytopdf-plugin-clip
COPY --from=binary --chmod=0755 /anytopdf-plugin-face-id /opt/anytopdf/plugins/anytopdf-plugin-face-id
COPY --from=binary --chmod=0755 /anytopdf-plugin-faces /opt/anytopdf/plugins/anytopdf-plugin-faces
COPY --from=binary --chmod=0755 /anytopdf-plugin-objects /opt/anytopdf/plugins/anytopdf-plugin-objects
COPY --from=binary --chmod=0755 /anytopdf-plugin-sentiment /opt/anytopdf/plugins/anytopdf-plugin-sentiment
COPY --from=binary --chmod=0755 /anytopdf-plugin-tika /opt/anytopdf/plugins/anytopdf-plugin-tika
COPY --from=binary --chmod=0755 /anytopdf-plugin-vlm /opt/anytopdf/plugins/anytopdf-plugin-vlm
COPY --from=binary --chmod=0755 /anytopdf-plugin-whisper /opt/anytopdf/plugins/anytopdf-plugin-whisper
COPY --from=whisper --chmod=0755 /out/bin/ /usr/local/bin/
ENV ANYTOPDF_WHISPER_MODEL=/models/ggml-base.en.bin
LABEL org.opencontainers.image.title="anytopdf" \
      org.opencontainers.image.description="Convert media and documents into searchable, RAG-friendly PDFs" \
      org.opencontainers.image.source="https://github.com/adeelahmad/anytopdf-rs" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
USER anytopdf
WORKDIR /work
ENTRYPOINT ["anytopdf"]
CMD ["--help"]
