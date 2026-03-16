# Stage 1: Build the safeclaw binary (glibc/Debian for CUDA compatibility)
FROM rust:1.93-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev make perl && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY config/ config/
COPY config.example.toml ./

# Cache buster — pass --build-arg CACHEBUST=$(date +%s) to force rebuild
ARG CACHEBUST=1
ARG CARGO_FEATURES=""
RUN cargo build --release --features "${CARGO_FEATURES}"

# Stage 2: Runtime (Debian slim)
# NOTE: Docker is optional. The safeclaw binary self-sandboxes on startup
# using Landlock, seccomp-bpf, and capability dropping (Linux) or Seatbelt
# (macOS). Running natively without Docker provides equivalent isolation.
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git bash \
    nodejs npm python3 python3-pip python3-venv && \
    rm -rf /var/lib/apt/lists/*

# Install Claude Code CLI globally
RUN npm install -g @anthropic-ai/claude-code

# Install ngrok
RUN ARCH="$(uname -m)" && \
    if [ "$ARCH" = "x86_64" ]; then NGROK_ARCH="amd64"; \
    elif [ "$ARCH" = "aarch64" ]; then NGROK_ARCH="arm64"; \
    else NGROK_ARCH="amd64"; fi && \
    curl -fsSL "https://ngrok-agent.s3.amazonaws.com/pool/main/n/ngrok/ngrok_3.36.1-0_${NGROK_ARCH}.deb" \
    -o /tmp/ngrok.deb && \
    dpkg -i /tmp/ngrok.deb && \
    rm /tmp/ngrok.deb

# Install common Python packages that skills are likely to need
RUN pip install --no-cache-dir --break-system-packages \
    requests \
    google-api-python-client \
    google-auth-httplib2 \
    google-auth-oauthlib \
    python-dotenv \
    schedule \
    httpx \
    beautifulsoup4 \
    feedparser \
    icalendar

# Copy safeclaw binary
COPY --from=builder /build/target/release/safeclaw /usr/local/bin/safeclaw

# Non-root user
ARG SAFE_UID=1000
ARG SAFE_GID=1000
RUN groupadd -g "${SAFE_GID}" safeclaw && \
    useradd -u "${SAFE_UID}" -g safeclaw -m -d /home/safeclaw -s /bin/bash safeclaw

RUN mkdir -p /data/safeclaw/skills /config/safeclaw /home/safeclaw && \
    chown -R safeclaw:safeclaw /data/safeclaw /config/safeclaw /home/safeclaw

ENV XDG_DATA_HOME=/data
ENV XDG_CONFIG_HOME=/config
ENV HOME=/home/safeclaw

EXPOSE 3031 443

VOLUME ["/data/safeclaw", "/config/safeclaw"]

USER safeclaw
ENTRYPOINT ["safeclaw"]
