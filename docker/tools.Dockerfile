# kobload-tools: pre-built base image with heavy runtime dependencies.
# Avoids re-downloading large packages on every --no-cache build.
#
# Rebuild with: make tools

FROM rust:1.99-trixie

# Install runtime dependencies:
# - ca-certificates: for HTTPS requests
# - unrar: for extracting RAR archives (requires non-free repo)
# - 7zip: for extracting 7z archives
# - curl + xz-utils: for downloading ffmpeg shared build
# - cmake + libfftw3-dev: for building chromaprint from source
# - pkg-config + libclang-dev: for kob-ffmpeg-sys bindgen at compile time
RUN echo "deb http://deb.debian.org/debian trixie non-free" >> /etc/apt/sources.list \
    && apt-get update && apt-get install -y \
    ca-certificates \
    cmake \
    curl \
    libclang-dev \
    libfftw3-dev \
    7zip \
    pkg-config \
    unrar \
    xz-utils \
    && rm -rf /var/lib/apt/lists/*

# Install latest FFmpeg shared build from BtbN/FFmpeg-Builds.
# Provides shared libraries + headers for kob-ffmpeg (replaces mediainfo CLI).
ARG TARGETARCH
RUN case "$TARGETARCH" in arm64) build=linuxarm64 ;; *) build=linux64 ;; esac \
    && curl -sL "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-$build-gpl-shared.tar.xz" \
    | tar -xJ --strip-components=1 -C /usr/local/ \
    && ldconfig

# Build chromaprint 1.6.0 from source (required by kob-ffmpeg-sys).
RUN curl -sL https://github.com/acoustid/chromaprint/releases/download/v1.6.0/chromaprint-1.6.0.tar.gz \
    | tar -xz \
    && cd chromaprint-1.6.0 \
    && cmake -B build -DCMAKE_INSTALL_PREFIX=/usr/local -DCMAKE_BUILD_TYPE=Release -DBUILD_TOOLS=OFF -DBUILD_SHARED_LIBS=ON \
    && cmake --build build --parallel \
    && cmake --install build \
    && cd / && rm -rf chromaprint-1.6.0 \
    && ldconfig

# Chromaprint cmake doesn't install a pkg-config file — create one manually.
RUN test -f /usr/local/lib/pkgconfig/libchromaprint.pc || \
    printf 'prefix=/usr/local\nexec_prefix=${prefix}\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\n\nName: chromaprint\nDescription: Audio fingerprint library\nVersion: 1.6.0\nLibs: -L${libdir} -lchromaprint\nCflags: -I${includedir}\n' \
    > /usr/local/lib/pkgconfig/libchromaprint.pc

# Ensure pkg-config finds the FFmpeg + chromaprint .pc files
ENV PKG_CONFIG_PATH="/usr/local/lib/pkgconfig"

# Install cargo-watch for dev hot-reload
RUN cargo install cargo-watch
