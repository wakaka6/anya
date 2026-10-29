# The Linux build environment: the system libraries Anya's Linux adapters and
# desktop shell link against. Used by scripts/linux-cargo.
# mirror.gcr.io serves Docker Hub's official images where Docker Hub is slow to reach.
FROM mirror.gcr.io/library/rust:1.95-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config clang \
        libasound2-dev \
        libx11-dev libxcb1-dev libxkbcommon-dev libxtst-dev \
        libwayland-dev \
        libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev \
    && rm -rf /var/lib/apt/lists/*
RUN rustup toolchain install 1.95 --profile minimal -c rustfmt,clippy
