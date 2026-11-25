# CI image: Rust runs here, built with docker buildx on GitHub-hosted runners (ubuntu-24.04 for
# x86_64, ubuntu-24.04-arm for the native arm64 stages).
# The crate depends on dobbo-ca/deflate-rs by git rev; a read-only GitHub App token fetches it
# while that repo is private. Its fetch is the only step that needs the GitHub App token, passed as
# the BuildKit secret `gh_token` (never a build arg, never written to a layer); every later step
# runs offline. The fuzz crate's own dependencies (libfuzzer-sys from crates.io) are fetched in the
# same step.
# Every tool is installed in a stage before `COPY . .`, so a new commit does not install it again,
# and the fetch runs from the manifests and lockfiles alone, so a commit that leaves them alone
# does not fetch again.
FROM rust:1.99.1-bookworm AS toolchain
ARG TARGETARCH
# On x86_64, gcc-aarch64-linux-gnu is the linker of `cargo build --target aarch64-unknown-linux-gnu`
# (the aarch64 lint and link check in `ci`). On arm64 that target is the host: no cross toolchain.
RUN rustup component add rustfmt clippy && rustup target add aarch64-unknown-linux-gnu \
    && if [ "$TARGETARCH" = amd64 ]; then \
         apt-get update && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
         && rm -rf /var/lib/apt/lists/* \
         && printf '[target.aarch64-unknown-linux-gnu]\nlinker = "aarch64-linux-gnu-gcc"\n' >> "$CARGO_HOME/config.toml"; \
       fi
WORKDIR /src

# Only the manifests, with empty targets for what they name, so cargo can resolve them.
FROM toolchain AS deps
COPY Cargo.toml Cargo.lock ./
COPY tests/fuzz/Cargo.toml tests/fuzz/Cargo.lock tests/fuzz/
RUN mkdir -p src tests/fuzz/fuzz_targets && touch src/lib.rs \
    && for t in exif xmp jpeg png webp read strip; do touch tests/fuzz/fuzz_targets/$t.rs; done
RUN --mount=type=secret,id=gh_token git config --global url."https://x-access-token:$(cat /run/secrets/gh_token)@github.com/".insteadOf "https://github.com/" && CARGO_NET_GIT_FETCH_WITH_CLI=true cargo fetch --locked && CARGO_NET_GIT_FETCH_WITH_CLI=true cargo fetch --locked --manifest-path tests/fuzz/Cargo.toml && git config --global --unset-all url."https://x-access-token:$(cat /run/secrets/gh_token)@github.com/".insteadOf \
    && rm -rf src tests/fuzz/fuzz_targets

FROM toolchain AS src
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .

# The checks run in the build, so a red check fails the build; a rerun of the same source is a
# BuildKit cache hit. The target dirs are cache mounts, so they persist across builds. The target
# cache is shared by every branch and cargo compares mtimes: another branch's newer build would be
# reused as this one's, so every RUN that runs cargo touches the sources just before it starts,
# and holds the cache with sharing=locked, so a concurrent build of another commit cannot build
# into it between the touch and cargo. No SIMD here, so there is no scalar-tier run.
FROM src AS ci
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo fmt --check
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo clippy --locked --keep-going --all-targets -- -D warnings
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo clippy --locked --keep-going --all-targets --target aarch64-unknown-linux-gnu -- -D warnings
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo test --locked
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo build --locked --release --target aarch64-unknown-linux-gnu
# The fuzz crate (libfuzzer-sys) has to compile; fuzz.yml runs its targets.
RUN --mount=type=cache,id=exif-fuzz-target,target=/src/tests/fuzz/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cd tests/fuzz && cargo check --locked --bins

# The arm64 stages are built natively on an arm64 runner (ubuntu-24.04-arm): no emulator.
FROM toolchain AS native-arm64
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .

# The whole suite runs on aarch64, released: the crate is small and has no SIMD, so
# there are no kernel-only tests to select (what the target changes is deflate-rs's NEON adler32,
# which every PNG text inflate runs).
FROM native-arm64 AS arm64
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + && cargo test --locked --release --target aarch64-unknown-linux-gnu

# libultrahdr's ultrahdr_app (google/libultrahdr v2.0.2, Apache-2.0 and MIT), a CI-only oracle: the
# parity stage makes the Ultra HDR test files with it and decodes them (tests/parity/gainmap.py).
# Never a crate dependency; no code of it is copied. Built with XMP on (UHDR_WRITE_XMP, off by
# default), so the primary image carries hdrgm:Version and a Container:Directory, and against the
# system libjpeg-turbo, so nothing else is fetched.
FROM rust:1.99.1-bookworm AS ultrahdr
RUN apt-get update && apt-get install -y --no-install-recommends cmake ninja-build pkg-config g++ libjpeg62-turbo-dev \
    && rm -rf /var/lib/apt/lists/*
RUN git init -q /uhdr && cd /uhdr \
    && git remote add origin https://github.com/google/libultrahdr \
    && git fetch -q --depth 1 origin e5f5a022fe96fc4dc2ee35c19f733a50df807abe \
    && git checkout -q FETCH_HEAD \
    && cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DUHDR_BUILD_DEPS=0 -DUHDR_ENABLE_HEIF=0 \
       -DUHDR_BUILD_TESTS=0 -DUHDR_WRITE_XMP=1 -DUHDR_WRITE_ISO=1 \
    && cmake --build build --target ultrahdr_app \
    && cp build/ultrahdr_app /usr/local/bin/ultrahdr_app \
    && cd / && rm -rf /uhdr

# The ExifTool oracle (perl is in the base image) and the Pillow that tests/parity/compare.py and
# strip_check.py use, and the libjpeg-turbo that ultrahdr_app links. CI-only, installed before
# `COPY . .`; never a crate dependency.
FROM toolchain AS oracle-tools
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-pil git ca-certificates libjpeg62-turbo \
    && rm -rf /var/lib/apt/lists/*

# ExifTool parity: fetch.sh puts ExifTool 13.64 and the metadata-extractor-images corpus into the
# shared /corpus cache mount (once per BuildKit cache), run.sh compares examples/oracle.rs against
# ExifTool over it and writes /report (summary.md, status). The step never fails on a parity
# difference, so the report is kept; the workflow fails the job when /report/status is not 0 (if
# run.sh wrote no status, the status of its exit is written here).
FROM oracle-tools AS parity
COPY --from=ultrahdr /usr/local/bin/ultrahdr_app /usr/local/bin/ultrahdr_app
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    --mount=type=cache,id=exif-corpus,target=/corpus,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + \
    && sh tests/parity/fetch.sh /corpus \
    && cargo build --locked --release --example oracle \
    && { sh tests/parity/run.sh /corpus target/release/examples/oracle /report; st=$?; \
         mkdir -p /report; [ -s /report/status ] || echo $st > /report/status; }

# The report as build output: docker/build-push-action with `outputs: type=local,dest=parity-out`.
FROM scratch AS parity-report
COPY --from=parity /report /

# The oracle example built for x86_64 reads every 8th input and strips it in all four modes; the
# outputs are the reference trees for parity-arm64.
FROM toolchain AS x86-ref
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    --mount=type=cache,id=exif-corpus,target=/corpus,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + \
    && sh tests/parity/fetch.sh /corpus \
    && mkdir -p /work /ref \
    && sh tests/parity/inputs.sh /corpus /work/in 8 \
    && echo "$(ls /work/in | wc -l) inputs" \
    && cargo build --locked --release --example oracle \
    && x=target/release/examples/oracle \
    && $x read /work/in /ref/x.jsonl \
    && for m in all keep default orient1; do $x strip $m /work/in /ref/x-$m || exit 1; done

# The x86_64 reference trees as a build output (job parity-ref uploads them for parity-arm64).
FROM scratch AS x86-ref-out
COPY --from=x86-ref /ref /

# The same oracle example built natively for aarch64 reads the same inputs and strips them in all
# four modes; the outputs must be byte-identical to the x86_64 trees of `x86-ref-out`. The workflow
# passes them as the named build context `x86ref`. Outputs are deleted at the end of the RUN, so the
# layer stays small.
FROM native-arm64 AS parity-arm64
RUN --mount=type=bind,from=x86ref,target=/ref \
    --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    --mount=type=cache,id=exif-corpus,target=/corpus,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + \
    && sh tests/parity/fetch.sh /corpus \
    && mkdir -p /work /out \
    && sh tests/parity/inputs.sh /corpus /work/in 8 \
    && echo "$(ls /work/in | wc -l) inputs" \
    && cargo build --locked --release --target aarch64-unknown-linux-gnu --example oracle \
    && a=target/aarch64-unknown-linux-gnu/release/examples/oracle \
    && $a read /work/in /out/a.jsonl \
    && diff /ref/x.jsonl /out/a.jsonl && echo "read: $(wc -l < /ref/x.jsonl) lines, identical" \
    && for m in all keep default orient1; do \
         $a strip $m /work/in /out/a-$m \
         && diff -r /ref/x-$m /out/a-$m && diff /ref/x-$m.errors /out/a-$m.errors \
         && echo "strip $m: $(ls /ref/x-$m | wc -l) files, identical" || exit 1; \
       done \
    && rm -rf /out /work/in

# Fuzzing (fuzz.yml): nightly and cargo-fuzz, with the targets built into the image. The workflow
# runs it with `docker run`, so a BuildKit cache hit cannot skip the fuzzing. No corpus in the
# image: the seeds are made by tests/fuzz/make-seeds.sh (printf, no binary fixtures in git).
FROM toolchain AS fuzz-tools
RUN rustup toolchain install nightly --profile minimal \
    && cargo install cargo-fuzz --locked --version 0.13.1

FROM fuzz-tools AS fuzz
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .
RUN cargo +nightly fuzz build --fuzz-dir tests/fuzz
RUN sh tests/fuzz/make-seeds.sh /seeds && find /seeds -type f | wc -l

# Benchmark (bench.yml): ExifTool and the Rust bench example over the same inputs in one image;
# tests/parity/bench.sh writes /report/bench.md. Information only. The workflow builds it with
# no-cache-filters: bench so the timing runs every time.
FROM oracle-tools AS bench
COPY --from=deps /usr/local/cargo/registry /usr/local/cargo/registry
COPY --from=deps /usr/local/cargo/git /usr/local/cargo/git
ENV CARGO_NET_OFFLINE=true
COPY . .
RUN --mount=type=cache,id=exif-target,target=/src/target,sharing=locked \
    --mount=type=cache,id=exif-corpus,target=/corpus,sharing=locked \
    find src examples tests -name '*.rs' -exec touch {} + \
    && sh tests/parity/fetch.sh /corpus \
    && sh tests/parity/inputs.sh /corpus /work/in \
    && cargo build --locked --release --example bench \
    && sh tests/parity/bench.sh /work/in /corpus/exiftool/exiftool target/release/examples/bench /report \
    && rm -rf /work/in

FROM scratch AS bench-report
COPY --from=bench /report /
