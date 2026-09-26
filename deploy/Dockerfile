# adk-runtime and the adk CLI in one small image.
#
# Build from the repository root (the context is the whole workspace):
#
#   docker build -f deploy/Dockerfile -t adk-runtime .
#
# or with Cloud Build: gcloud builds submit --config deploy/cloudbuild.yaml
#
# The image runs adk-runtime, configured by environment (ADK_BUNDLES,
# ADK_SERVE, ADK_RUNTIME_TOKENS, ...); see docs/user-guide/deploy.md.

# The toolchain matches rust-toolchain.toml, so rustup has nothing to fetch.
FROM rust:1.93.1-slim-bookworm AS build
# The default TLS backend is OpenSSL: headers to build against.
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked \
      -p gemini-adk-server-rs --bin adk-runtime \
      -p gemini-adk-cli-rs --bin adk \
 && cp target/release/adk-runtime target/release/adk /usr/local/bin/

FROM debian:bookworm-slim
# ca-certificates: the TLS roots for Vertex AI, Cloud Storage, the metadata
# server's HTTPS peers and any HTTP tool a spec binds (both TLS stacks in
# the binary read the system store). libssl3: OpenSSL at run time.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home adk
COPY --from=build /usr/local/bin/adk-runtime /usr/local/bin/adk /usr/local/bin/
USER 10001
ENV PORT=8080
EXPOSE 8080
ENTRYPOINT ["adk-runtime"]
