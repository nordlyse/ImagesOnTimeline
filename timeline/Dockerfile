FROM rust:1-alpine AS build

RUN apk add --no-cache musl-dev

WORKDIR /src
COPY timeline/Cargo.toml timeline/Cargo.lock timeline/
COPY timeline/src timeline/src
COPY timeline/static timeline/static

WORKDIR /src/timeline
RUN cargo build --release --locked

FROM scratch

COPY --from=build /src/timeline/target/release/timeline /timeline

ENV PORT=8080
ENV LIBRARY_ROOT=/host
EXPOSE 8080
ENTRYPOINT ["/timeline"]
