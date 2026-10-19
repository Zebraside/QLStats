# syntax=docker/dockerfile:1

# Сборка идёт на архитектуре хоста (BUILDPLATFORM) и кросс-компилируется под
# целевую (TARGETARCH: amd64 или arm64), поэтому multi-arch образ собирается
# без эмуляции: docker buildx build --platform linux/amd64,linux/arm64 ...
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
ARG BUILDARCH
ARG TARGETARCH

# Кросс-компиляторы C/C++: крейт zmq собирает libzmq (C++), rusqlite — SQLite (C).
# Если целевая архитектура совпадает с хостовой, кросс-пакет не нужен (и в Debian
# для своей архитектуры его нет): хватает нативного gcc, у которого уже есть
# симлинки вида x86_64-linux-gnu-gcc.
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) pkg=g++-x86-64-linux-gnu;  target=x86_64-unknown-linux-gnu ;; \
      arm64) pkg=g++-aarch64-linux-gnu; target=aarch64-unknown-linux-gnu ;; \
      *) echo "неподдерживаемая архитектура: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends "$pkg"; \
      rm -rf /var/lib/apt/lists/*; \
    fi; \
    rustup target add "$target"; \
    echo "$target" > /rust-target

ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static

# Кэш реестра crates.io и каталога target переживает пересборки (BuildKit).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=qlstats-target-$TARGETARCH \
    set -eux; \
    target="$(cat /rust-target)"; \
    cargo build --release --locked --target "$target"; \
    install -D -m 755 "target/$target/release/qlstats" /out/qlstats

# Минимальный рантайм: glibc, libstdc++ (нужна libzmq), CA-сертификаты; без shell.
# Пользователь nonroot (uid/gid 65532).
FROM gcr.io/distroless/cc-debian12:nonroot

LABEL org.opencontainers.image.title="qlstats" \
      org.opencontainers.image.description="Статистика игроков Quake Live по ZMQ-потоку"

COPY --from=builder /out/qlstats /usr/local/bin/qlstats

# /data — том с конфигом и базой. Рабочий каталог тоже /data, поэтому
# относительные пути в конфиге и ./.env считаются от него.
ENV QLSTATS_CONFIG=/data/config.toml
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/qlstats"]
CMD ["run"]
