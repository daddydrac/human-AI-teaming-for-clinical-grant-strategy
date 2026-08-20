import os
import threading
from typing import List, Union

import numpy as np
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field
from fastembed import TextEmbedding

MODEL_NAME = os.getenv("CPU_EMBEDDING_MODEL", "BAAI/bge-small-en-v1.5")
API_MODEL = os.getenv("CPU_EMBEDDING_API_MODEL", "grant-embedding-cpu")
CACHE_DIR = os.getenv("FASTEMBED_CACHE_PATH", "/models")
THREADS = max(1, int(os.getenv("CPU_EMBEDDING_THREADS", "2")))
BATCH_SIZE = max(1, int(os.getenv("CPU_EMBEDDING_BATCH_SIZE", "16")))

app = FastAPI(title="Grant Writer CPU Embeddings", docs_url=None, redoc_url=None)
_model_lock = threading.Lock()
_model: TextEmbedding | None = None


def get_model() -> TextEmbedding:
    global _model
    if _model is None:
        with _model_lock:
            if _model is None:
                os.makedirs(CACHE_DIR, exist_ok=True)
                _model = TextEmbedding(model_name=MODEL_NAME, cache_dir=CACHE_DIR, threads=THREADS)
    return _model


class EmbeddingRequest(BaseModel):
    model: str = Field(min_length=1)
    input: Union[str, List[str]]


@app.get("/health")
def health():
    model = get_model()
    vector = next(model.embed(["health check"], batch_size=1))
    return {"status": "ok", "model": API_MODEL, "source_model": MODEL_NAME, "dimensions": int(vector.shape[0]), "threads": THREADS}


@app.get("/v1/models")
def models():
    return {"object": "list", "data": [{"id": API_MODEL, "object": "model", "owned_by": "local-fastembed"}]}


@app.post("/v1/embeddings")
def embeddings(req: EmbeddingRequest):
    if req.model not in {API_MODEL, MODEL_NAME}:
        raise HTTPException(status_code=404, detail=f"embedding model '{req.model}' is not served")
    items = [req.input] if isinstance(req.input, str) else req.input
    if not items:
        return {"object": "list", "data": [], "model": API_MODEL, "usage": {"prompt_tokens": 0, "total_tokens": 0}}
    if any(not isinstance(x, str) or not x.strip() for x in items):
        raise HTTPException(status_code=400, detail="all embedding inputs must be non-empty strings")
    model = get_model()
    vectors = list(model.embed(items, batch_size=min(BATCH_SIZE, len(items))))
    data = []
    for idx, vec in enumerate(vectors):
        arr = np.asarray(vec, dtype=np.float32)
        data.append({"object": "embedding", "index": idx, "embedding": arr.tolist()})
    return {"object": "list", "data": data, "model": API_MODEL, "usage": {"prompt_tokens": 0, "total_tokens": 0}}
