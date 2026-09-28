FROM docker.io/cloudflare/sandbox:0.12.10
RUN apt-get update && apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev ca-certificates curl && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.93.1 && chmod -R a+rX /opt/rustup /opt/cargo
RUN npm install -g @openai/codex@0.157.1 @anthropic-ai/claude-code@2.1.283 @github/copilot@1.0.88
RUN getent passwd 1000 || useradd -u 1000 -m wreckit
COPY runner /opt/wreck-it
