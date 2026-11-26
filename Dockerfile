# SPDX-License-Identifier: AGPL-3.0-or-later
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked && strip target/release/meshkeep

FROM scratch
COPY --from=build /src/target/release/meshkeep /meshkeep
VOLUME /data
EXPOSE 5000 8080
ENTRYPOINT ["/meshkeep", "--data-dir", "/data"]
