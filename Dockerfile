# syntax=docker/dockerfile:1
#
# anytopdf with FFmpeg, ExifTool, Tesseract and a Unicode font.
#
#   docker build -t anytopdf .
#   docker run --rm -v "$PWD:/work" anytopdf notes.txt photo.jpg -o out.pdf
#   docker run --rm -i -v "$PWD:/work" anytopdf mcp
#
# The release workflow passes BINARY=prebuilt and copies the musl release
# binaries to dist/docker/<arch>/anytopdf, so the image ships the exact binary
# that was smoke-tested and checksummed instead of rebuilding it.

ARG BINARY=source

FROM rust:1.88-bookworm AS source
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p anytopdf --no-default-features \
    && install -m 0755 target/release/anytopdf /anytopdf

FROM scratch AS prebuilt
ARG TARGETARCH
COPY dist/docker/${TARGETARCH}/anytopdf /anytopdf

FROM ${BINARY} AS binary

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates ffmpeg libimage-exiftool-perl tesseract-ocr tesseract-ocr-eng fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 1000 anytopdf
COPY --from=binary --chmod=0755 /anytopdf /usr/local/bin/anytopdf
LABEL org.opencontainers.image.title="anytopdf" \
      org.opencontainers.image.description="Convert media and documents into searchable, RAG-friendly PDFs" \
      org.opencontainers.image.source="https://github.com/adeelahmad/anytopdf-rs" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
USER anytopdf
WORKDIR /work
ENTRYPOINT ["anytopdf"]
CMD ["--help"]
