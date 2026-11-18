# helm-embed: an embedding server for CPUs without AVX

A small container that serves text embeddings over an OpenAI-style
`POST /v1/embeddings`, built on llama.cpp's `llama-server`. It exists for
containers running on low-power x86-64 CPUs (the Goldmont family and similar)
that have SSE4.2 but no AVX, AVX2 or FMA: llama.cpp built for the host would
not start there, and the default SIMD dispatch would crash on the first
instruction. This image builds with `GGML_NATIVE=OFF` and the AVX, AVX2, FMA
and F16C kernels switched off, so llama.cpp uses its SSE4.2 baseline kernels
everywhere. Slower, but correct, and the only thing it is ever asked to embed
is a single search query at a time (document embedding is done elsewhere).

## Build

The build context is this folder:

```
docker build -t helm-embed:latest hub/embedder/
```

llama.cpp is pinned by release tag (`ARG LLAMA_TAG`, default `b12414`) so a
rebuild is reproducible. Bump the tag deliberately and re-check the vectors
(see Verify below); an unpinned `master` clone would silently change results.

## The model is mounted, not baked in

The image carries no model. Download the embedding model once into a folder
on the host and mount it at `/models`:

- file: `nomic-embed-text-v1.5.Q8_0.gguf` (768-dimensional, Q8_0 quantisation)
- source: <https://huggingface.co/nomic-ai/nomic-embed-text-v1.5-GGUF/resolve/main/nomic-embed-text-v1.5.Q8_0.gguf>
- size: 146,146,432 bytes
- sha256: `3e24342164b3d94991ba9692fdc0dd08e3fd7362e0aacc396a9a5c54a544c3b7`

Check the checksum after copying; a truncated download on a shared network
drive is a classic silent failure. The container fails fast if the file is
missing (see the run example). The path can be moved with `EMBED_MODEL`.

## Run

It is meant to sit on an internal container network beside its caller, with
no published host port:

```yaml
services:
  helm-embed:
    image: helm-embed:latest
    volumes:
      - ./models:/models:ro
    mem_limit: 512m
    cpus: 1.0
    restart: unless-stopped
```

The caller reaches it at `http://helm-embed:8080` on the shared network.

The server starts with `--threads 2 --parallel 1 --ctx-size 512`: two compute
threads, one request slot, a short context. These are not tunables to raise on
a whim — on a four-core shared machine this is the budget one query embed
should cost. Second requests queue; callers are expected to time out rather
than wait indefinitely.

## Endpoints

- `POST /v1/embeddings` — `{"model": "...", "input": "text"}`; the model name
  is ignored, there is only the mounted one.
- `GET /health` — what the container `HEALTHCHECK` uses.
- `GET /props` — reports the model, context size and whether embeddings are
  on; useful for a smoke test.

## Verify after a rebuild

Vectors from different llama.cpp builds or quantisations must stay
comparable with whoever reads them. After bumping `LLAMA_TAG`, embed the same
50 sample texts with this image and with the embedding server that produced
the stored document vectors, and check the cosine of each pair is ≥ 0.99
before deploying.
