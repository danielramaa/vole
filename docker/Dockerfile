# Arch Linux dev environment for vole: same toolchain on any host (Linux, Windows, macOS).
# vole only runs on Arch, so this is also a throwaway system to test cleaning safely.
FROM archlinux:latest

RUN pacman -Syu --noconfirm --needed base-devel git rustup pacman-contrib sudo \
    && pacman -Scc --noconfirm

# A normal user, like on a real system: vole refuses to run as root.
# The password is "dev", so sudo really asks for it, like vole's sudo prompts will.
ARG UID=1000
ARG GID=1000
RUN groupadd -g "$GID" dev \
    && useradd -m -u "$UID" -g "$GID" -s /bin/bash dev \
    && echo 'dev:dev' | chpasswd \
    && echo 'dev ALL=(ALL) ALL' > /etc/sudoers.d/dev

USER dev
RUN rustup default stable \
    && rustup component add rustfmt clippy \
    && mkdir -p /home/dev/.cargo/registry /home/dev/target

# Build output stays inside the container (faster than the shared folder, and keeps
# Linux build files out of your host's target/).
ENV CARGO_TARGET_DIR=/home/dev/target
WORKDIR /home/dev/vole
CMD ["bash"]
