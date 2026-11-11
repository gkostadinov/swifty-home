# syntax=docker/dockerfile:1
#
# Swifty Home as a container image: one static binary on a distroless base, run as a
# non-root user (uid and gid 65532).
#
# Build from the root of the repository:
#   docker build -f packaging/docker/Dockerfile -t swifty-home .
#
# Each architecture builds natively (linux/amd64, linux/arm64). The release workflow takes
# the binary for each release archive out of this same build, so the archive and the image
# carry the same binary.

ARG RUST_VERSION=1.98.1

FROM rust:${RUST_VERSION}-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
# The musl target links statically by default. Naming it with --target keeps build scripts
# and proc-macros on the host's own settings.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    set -eu; \
    target="$(rustc -vV | sed -n 's/^host: //p')"; \
    cargo build --release --locked --package swifty --bin swifty --target "$target"; \
    install -D -m 0755 "target/$target/release/swifty" /out/bin/swifty; \
    /out/bin/swifty --help >/dev/null; \
    install -d -m 0750 /out/etc/swifty-home; \
    install -d -m 0700 /out/var/lib/swifty-home

FROM gcr.io/distroless/static-debian12:nonroot
LABEL org.opencontainers.image.title="Swifty Home" \
      org.opencontainers.image.description="A self-hosted smart home hub" \
      org.opencontainers.image.licenses="Apache-2.0"
COPY --from=build /out/bin/swifty /usr/local/bin/swifty
# Copying the contents of /out/etc and /out/var/lib creates the two directories owned by
# the non-root user, so named volumes mounted there start out writable by it.
COPY --from=build --chown=65532:65532 /out/etc/ /etc/
COPY --from=build --chown=65532:65532 /out/var/lib/ /var/lib/
USER 65532:65532
# The hub listens on 127.0.0.1:8790 unless swifty.toml says otherwise. Run it with host
# networking (see docker-compose.yml).
EXPOSE 8790
ENTRYPOINT ["/usr/local/bin/swifty"]
CMD ["serve", "--config", "/etc/swifty-home/swifty.toml"]
