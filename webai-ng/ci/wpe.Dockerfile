# webai-ng legacy_cpp CI image (task: WPE container image)
#
# Reproducible WPE stack for the `legacy_cpp` build/test path. Uses Debian
# distro packages (libwpewebkit) instead of compiling WPE WebKit from source
# (ARCHITECTURE.md §10 warns the cmake build is prohibitively expensive).
#
# Tag convention: orlfly/webai-ng-ci:wpe-<WPEWEBKIT-MAJOR.MINOR>-rust<RUST_MINOR>
# e.g. wpe-2.48-rust90. The MAJOR.MINOR reflects the ACTUAL libwpewebkit
# version installed by the Debian bookworm package (Major-1 on #35: the tag
# must match reality so upgrade triage is not misled).
# Upgrade flow (see webai-ng/docs/wpe-image.md): bump libwpewebkit package,
# rebuild, re-tag, update ci.yml, run the smoke job once before merging.
#
# Build:  docker build -t orlfly/webai-ng-ci:wpe-2.48-rust90 -f webai-ng/ci/wpe.Dockerfile webai-ng/
# Cold-build timing + cache policy are documented in webai-ng/docs/wpe-image.md.

FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive
# Headless default display backend (Major-2 on #35): cog launch in the
# headless CI container needs WPE_BACKEND set so WPEBackend-fdo renders
# off-screen; xvfb provides the X display cog may require.
ENV WPE_BACKEND=fdo

# Layer 1: WPE runtime + build deps (apt-cached; versions pinned by bookworm
# release, currently WPE WebKit 2.48/2.50 line — exact versions recorded in
# webai-ng/docs/wpe-image.md at image release time).
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git build-essential pkg-config libclang-dev clang \
    python3 procps cmake ninja-build \
    libwpe-1.0-1 libwpe-1.0-dev \
    libwpebackend-fdo-1.0-1 libwpebackend-fdo-1.0-dev \
    libwpewebkit-1.0-1 libwpewebkit-1.0-dev \
    cog \
    libssl3 libssl-dev \
    libglib2.0-dev libgtk-3-dev \
    xvfb xauth \
    && rm -rf /var/lib/apt/lists/*

# Layer 2: Rust toolchain (1.87+ per workspace rust-version).
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain 1.90.0 --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /work
# Cache-friendly: source is bind-mounted by CI, nothing copied so the image
# itself stays small and rebuilds only when apt or the toolchain changes.
CMD ["bash"]
