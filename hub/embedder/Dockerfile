# helm-embed: a small embedding server for x86-64 containers running on CPUs
# without AVX. llama.cpp is built with GGML_NATIVE off and the AVX, AVX2, FMA
# and F16C kernels disabled, so it runs its SSE4.2 baseline kernels everywhere.
# The model is mounted at run time, never baked into the image.
# Build context: hub/embedder/.

FROM debian:bookworm AS build
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
         build-essential cmake git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Pinned for reproducibility: bump deliberately, never implicitly.
ARG LLAMA_TAG=b12414
RUN git clone --depth 1 --branch "$LLAMA_TAG" \
      https://github.com/ggml-org/llama.cpp /src/llama.cpp

WORKDIR /src/llama.cpp
# BMI2 is off because the server this runs on has an older CPU without it. The web UI is not
# fetched at build time (the pinned tag's download address no longer resolves, and the
# embedder needs no UI).
RUN cmake -B build -DCMAKE_BUILD_TYPE=Release \
      -DLLAMA_CURL=OFF \
      -DGGML_OPENMP=OFF \
      -DGGML_NATIVE=OFF \
      -DGGML_AVX=OFF \
      -DGGML_AVX2=OFF \
      -DGGML_FMA=OFF \
      -DGGML_F16C=OFF \
      -DGGML_BMI2=OFF \
      -DLLAMA_USE_PREBUILT_UI=OFF \
    && cmake --build build --config Release --target llama-server -j "$(nproc)" \
    && mkdir -p /opt/llama \
    && cp -a build/bin/llama-server build/bin/*.so* /opt/llama/

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends wget \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /models && chown 1000:100 /models
# llama-server is a thin launcher: it dlopens libllama-server-impl.so by bare
# name and links against the versioned llama/ggml/mtmd shared libraries, so
# everything build/bin produces must ship in one folder together.
COPY --from=build /opt/llama/ /usr/local/bin/
USER 1000:100
ENV EMBED_MODEL=/models/nomic-embed-text-v1.5.Q8_0.gguf
# llama-server dlopens libllama-server-impl.so by bare name, so its folder
# must be on the loader path.
ENV LD_LIBRARY_PATH=/usr/local/bin
VOLUME /models
EXPOSE 8080
# The defaults are sized for a shared machine with a fractional CPU: two
# compute threads, one slot, a short context. Raise them only with a plan for
# the memory and CPU that costs.
ENTRYPOINT ["sh", "-c", "exec llama-server --model \"$EMBED_MODEL\" --embedding --host 0.0.0.0 --port 8080 --threads 2 --parallel 1 --ctx-size 512"]
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=20s \
  CMD wget -qO- http://127.0.0.1:8080/health >/dev/null || exit 1
