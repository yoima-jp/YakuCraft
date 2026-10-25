FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/root/.cargo \
    CARGO_TARGET_DIR=/opt/yakucraft-cargo-target \
    RUSTUP_HOME=/root/.rustup \
    PATH=/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update && apt-get install --no-install-recommends -y \
    build-essential \
    ca-certificates \
    curl \
    dbus-x11 \
    file \
    fonts-noto-cjk \
    gnupg \
    gnome-keyring \
    libayatana-appindicator3-dev \
    libfuse2 \
    libgl1-mesa-dri \
    libssl-dev \
    librsvg2-dev \
    libwebkit2gtk-4.1-dev \
    libxdo-dev \
    patchelf \
    pkg-config \
    squashfs-tools \
    webkit2gtk-driver \
    wget \
    xdg-utils \
    xvfb \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_24.x -o /tmp/nodesource_setup.sh \
    && bash /tmp/nodesource_setup.sh \
    && apt-get install --no-install-recommends -y nodejs \
    && rm -f /tmp/nodesource_setup.sh \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain stable --component clippy --component rustfmt \
    && rm -f /tmp/rustup-init.sh

RUN cargo install --locked tauri-driver

WORKDIR /workspace
CMD ["bash", "scripts/linux-verify/verify.sh"]
