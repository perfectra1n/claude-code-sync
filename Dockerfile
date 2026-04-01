# Build stage
FROM rust:1.94-slim AS builder

WORKDIR /app

# Cache dependency builds
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "" > src/lib.rs
RUN cargo build --release && rm -rf src

# Build the actual binary
COPY src/ src/
RUN touch src/main.rs src/lib.rs
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/claude-code-sync /usr/local/bin/claude-code-sync

ENTRYPOINT ["claude-code-sync"]
