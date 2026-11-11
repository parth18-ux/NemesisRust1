# One image for the API and the scoring workers.
#   docker build -f deploy/Dockerfile -t rusetect .
FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --release -p rusetect-server --bins

FROM debian:stable-slim
WORKDIR /app
COPY --from=build /src/target/release/rusetect-server /src/target/release/rusetect-worker /src/target/release/rusetect-replay /usr/local/bin/
COPY service service
COPY fixtures fixtures
COPY results results
ENV RUSETECT_ROOT=/app HOST=0.0.0.0 PORT=8000
EXPOSE 8000
CMD ["rusetect-server"]
