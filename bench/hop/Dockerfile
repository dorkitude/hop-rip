# hop, built from this checkout, on a minimal glibc base (the binary is dynamically linked).
FROM rust:1.99-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY static static
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/hop /usr/local/bin/hop
ENTRYPOINT ["/usr/local/bin/hop", "serve"]
