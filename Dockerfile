FROM docker.io/library/rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN cargo build --release --locked && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /src/target/release/shinybox /usr/local/bin/shinybox
COPY --from=build --chown=nonroot:nonroot /data /data
ENV SHINYBOX_BIND=0.0.0.0:8080 \
    SHINYBOX_DATA_DIR=/data
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/shinybox"]
