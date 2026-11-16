# Build and runtime images are pinned to the same Debian release so the
# binary's glibc matches. Mixing, say, a bookworm build with a trixie runtime
# is the usual cause of a "version `GLIBC_2.xx' not found" crash on boot.
FROM rust:1-slim-bookworm AS build
WORKDIR /src

# Dependencies first so edits to src/ do not invalidate the cached layer.
# There are no third-party crates, so this is just the manifest.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin battlesnake

FROM debian:bookworm-slim
RUN useradd --create-home --shell /usr/sbin/nologin snake
COPY --from=build /src/target/release/battlesnake /usr/local/bin/battlesnake
USER snake

# The server reads its configuration from the environment; see README.
ENV PORT=8000 \
    THREADS=4 \
    BUDGET_MARGIN_MS=120

EXPOSE 8000
CMD ["battlesnake"]
