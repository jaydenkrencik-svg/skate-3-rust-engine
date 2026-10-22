# Browser build + web server. Contains no game data: mount the `data`
# directory that setup produced from your own disc at /data.
#
#   docker build -f web/Dockerfile -t skate3-web .
#   docker run -p 8080:8080 -v /path/to/data:/data:ro -e SKATE_WEB_PASSWORD=... skate3-web
FROM rust:1-bookworm AS build
RUN rustup target add wasm32-unknown-unknown \
 && cargo install wasm-bindgen-cli --version 0.2.127 --locked \
 && apt-get update && apt-get install -y --no-install-recommends binaryen && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p skate-web-server \
 && cargo build --locked -p skate-game --bin skate3rust --no-default-features \
      --target wasm32-unknown-unknown --profile wasm-release \
 && mkdir -p web/dist/pkg \
 && wasm-bindgen --target web --no-typescript --out-dir web/dist/pkg \
      target/wasm32-unknown-unknown/wasm-release/skate3rust.wasm \
 && wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
      --enable-mutable-globals --enable-reference-types --enable-multivalue \
      -o web/dist/pkg/skate3rust_bg.wasm web/dist/pkg/skate3rust_bg.wasm \
 && cp -r web/static/* web/dist/ \
 && printf '{"revision":"%s","profile":"wasm-release"}\n' "$(git rev-parse HEAD 2>/dev/null || echo unknown)" > web/dist/build.json

FROM debian:bookworm-slim
COPY --from=build /src/target/release/skate-web-server /usr/local/bin/skate-web-server
COPY --from=build /src/web/dist /srv/web
ENV SKATE_WEB_BIND=0.0.0.0:8080 SKATE_WEB_DIR=/srv/web SKATE_WEB_DATA=/data
EXPOSE 8080
# The server refuses to listen on 0.0.0.0 without SKATE_WEB_PASSWORD.
CMD ["skate-web-server"]
