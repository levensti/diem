FROM debian:buster-20210927@sha256:37b68406b219ca10fd50a49340da9e6e9ac189deecf75c3d3bd90a016d29fbb0 AS debian-base

FROM debian-base AS toolchain

# To use http/https proxy while building, use:
# docker build --build-arg https_proxy=http://fwdproxy:8080 --build-arg http_proxy=http://fwdproxy:8080

RUN apt-get update && apt-get install -y cmake curl clang git pkg-config libssl-dev

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
ENV PATH "$PATH:/root/.cargo/bin"

WORKDIR /diem
COPY rust-toolchain /diem/rust-toolchain
RUN rustup install $(cat rust-toolchain)

FROM toolchain AS builder

ARG ENABLE_FAILPOINTS
COPY . /diem

RUN IMAGE_TARGETS="release" ./docker/build-common.sh

### Production Image ###
FROM debian-base AS prod

RUN apt-get update && apt-get install -y libssl1.1 && apt-get clean && rm -r /var/lib/apt/lists/*

RUN addgroup --system --gid 6180 diem && adduser --system --ingroup diem --no-create-home --uid 6180 diem

RUN mkdir -p /opt/diem/bin /opt/diem/etc /opt/diem/data

COPY --from=builder /diem/target/release/diem-key-manager /opt/diem/bin
COPY --from=builder /diem/target/release/safety-rules /opt/diem/bin

ENV RUST_BACKTRACE 1

ARG BUILD_DATE
ARG GIT_REV
ARG GIT_UPSTREAM

LABEL org.label-schema.schema-version="1.0"
LABEL org.label-schema.build-date=$BUILD_DATE
LABEL org.label-schema.vcs-ref=$GIT_REV
LABEL vcs-upstream=$GIT_UPSTREAM
