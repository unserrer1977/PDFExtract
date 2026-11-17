# PdfCraft is compiled in the image. The source tree is not committed to this repo.
FROM rust:1.95-bookworm AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    cmake \
    git \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
ARG PDFCRAFT_COMMIT=271e479c53723b1b294c866d5cb2f27604601b29
RUN git init \
    && git remote add origin https://github.com/storytold/pdfcraft.git \
    && git fetch --depth 1 origin ${PDFCRAFT_COMMIT} \
    && git checkout --detach FETCH_HEAD \
    && rm -rf .git

COPY service/pdfmd-core /src/apps/pdfmd-core
COPY service/pdfmd /src/apps/pdfmd

ENV CARGO_PROFILE_RELEASE_LTO=off \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_PROFILE_RELEASE_DEBUG=0 \
    CARGO_BUILD_JOBS=1
RUN cargo build -p pdfmd --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libgcc-s1 \
    libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 pdf

COPY --from=build /src/target/release/pdfmd /usr/local/bin/pdfmd
USER pdf
ENV PORT=8080
EXPOSE 8080
CMD ["pdfmd"]
